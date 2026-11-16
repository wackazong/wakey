//! Writing a download to the card while the next data arrives.

use crate::psram::PsramBuffer;
use crate::runtime::Thread;
use esp_idf_svc::hal::cpu::Core;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;

/// Bytes per card write. The card and the C6 share the SDMMC controller,
/// one transfer at a time: larger writes hold the C6 off long enough to
/// slow the download.
pub(super) const BLOCK: usize = 32 * 1024;
/// Blocks circulating between the download and the writer: when the card
/// falls behind, the download waits for a free one.
const BLOCKS: usize = 16;

/// Writes a file on its own thread (`card-writer`, on core 1 below the
/// audio decoder), in whole [`BLOCK`]s from cache-line aligned PSRAM: FatFs
/// hands such a block to the SD driver as is, which sends it by DMA
/// straight from PSRAM (from an unaligned buffer it copies every write
/// through a small bounce buffer). Network and card work at the same time.
///
/// [`finish`](Self::finish) writes the rest and reports the result; a
/// writer dropped before keeps what it wrote so far.
pub(super) struct CardWriter {
    /// Full blocks (address, length) to the writer; `None`: finish.
    to_writer: Option<mpsc::SyncSender<Option<(usize, usize)>>>,
    /// Written blocks back.
    free: mpsc::Receiver<usize>,
    writer: Option<JoinHandle<std::io::Result<()>>>,
    /// The block being filled (address), and how full.
    cur: usize,
    len: usize,
    /// All blocks, one allocation (freed after the writer is joined:
    /// fields drop after [`Drop::drop`]).
    _region: PsramBuffer,
}

impl CardWriter {
    /// Writes `path` from byte `at` on (0: a new file; else a resumed
    /// download, what follows `at` goes). `at` a multiple of [`BLOCK`]
    /// keeps the card writes aligned.
    pub(super) fn open(path: &Path, at: u64) -> std::io::Result<Self> {
        let mut file = if at == 0 {
            std::fs::File::create(path)?
        } else {
            let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
            f.set_len(at)?;
            f.seek(SeekFrom::Start(at))?;
            f
        };
        let region = PsramBuffer::new(BLOCK * BLOCKS, esp_idf_svc::sys::MALLOC_CAP_8BIT)
            .ok_or_else(|| std::io::Error::other("no memory for the write buffers"))?;
        let (to_writer, from_main) = mpsc::sync_channel::<Option<(usize, usize)>>(BLOCKS);
        let (to_main, free) = mpsc::sync_channel::<usize>(BLOCKS);
        let base = region.as_mut_ptr() as usize;
        for k in 1..BLOCKS {
            let _ = to_main.send(base + k * BLOCK);
        }
        let writer = Thread::new(c"card-writer", 6144)
            .core(Core::Core1)
            .priority(4)
            .spawn(move || {
                let mut blocks = 0u32;
                while let Ok(Some((addr, len))) = from_main.recv() {
                    // SAFETY: the block is this thread's until sent back,
                    // and `len` bytes of it are filled.
                    let data = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
                    file.write_all(data)?;
                    let _ = to_main.send(addr);
                    // FatFs updates a file's size on the card only on
                    // sync, and a reader opening it (a preview while
                    // downloading, `Growing`) sees that size: after the
                    // first block, then every 256 KiB.
                    blocks += 1;
                    if blocks == 1 || blocks.is_multiple_of(8) {
                        file.sync_all()?;
                    }
                }
                file.flush()
            })?;
        Ok(Self {
            to_writer: Some(to_writer),
            free,
            writer: Some(writer),
            cur: base,
            len: 0,
            _region: region,
        })
    }

    /// Queues `data`; waits only when all blocks are full.
    pub(super) fn write(&mut self, mut data: &[u8]) -> std::io::Result<()> {
        while !data.is_empty() {
            let take = (BLOCK - self.len).min(data.len());
            // SAFETY: `cur` is a block of BLOCK bytes owned by this side
            // until passed; `len + take` stays within it.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    (self.cur as *mut u8).add(self.len),
                    take,
                )
            };
            self.len += take;
            data = &data[take..];
            if self.len == BLOCK {
                self.pass()?;
            }
        }
        Ok(())
    }

    /// Writes what is left and waits until all is on the card.
    pub(super) fn finish(mut self) -> std::io::Result<()> {
        if self.len > 0 {
            self.send(Some((self.cur, self.len)))?;
            self.len = 0;
        }
        self.send(None)?;
        self.join()
    }

    /// Hands the current block to the writer and takes a free one.
    fn pass(&mut self) -> std::io::Result<()> {
        self.send(Some((self.cur, self.len)))?;
        self.len = 0;
        match self.free.recv() {
            Ok(b) => {
                self.cur = b;
                Ok(())
            }
            Err(_) => Err(self.stopped()),
        }
    }

    fn send(&mut self, msg: Option<(usize, usize)>) -> std::io::Result<()> {
        match &self.to_writer {
            Some(tx) if tx.send(msg).is_ok() => Ok(()),
            _ => Err(self.stopped()),
        }
    }

    /// The writer ended early: its error.
    fn stopped(&mut self) -> std::io::Error {
        self.join()
            .err()
            .unwrap_or_else(|| std::io::Error::other("card writer stopped"))
    }

    /// Ends the writer thread; its result.
    fn join(&mut self) -> std::io::Result<()> {
        self.to_writer = None;
        match self.writer.take() {
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("card writer panicked"))),
            None => Ok(()),
        }
    }
}

impl Drop for CardWriter {
    fn drop(&mut self) {
        // The writer must be done with the blocks before they go.
        let _ = self.join();
    }
}
