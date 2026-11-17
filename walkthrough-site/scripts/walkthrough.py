"""What the scripts share: where things are, and reading code excerpts
out of the content files."""

import re
import subprocess
import tomllib
from dataclasses import dataclass
from pathlib import Path

SITE = Path(__file__).resolve().parent.parent
CONTENT = SITE / "content"

# {% <code file="…" lines="…" hl="…"> %}{% raw %} … {% endraw %}{% </code> %}
EXCERPT = re.compile(
    r"\{%\s*<code\s+(?P<args>[^>]*?)>\s*%\}\{%\s*raw\s*%\}\n(?P<body>.*?)\n\{%\s*endraw\s*%\}\{%\s*</code>\s*%\}",
    re.DOTALL,
)
ARG = re.compile(r'(\w+)="([^"]*)"')
# A line that stands for lines left out; the excerpt goes on at line N.
GAP = re.compile(r"^\s*(//|#|/\*)\s*\.\.\.\s*(\*/\s*)?@(\d+)\s*$")


def front_matter(path: Path) -> dict:
    """The TOML front matter of a content file."""
    text = path.read_text(encoding="utf-8")
    if not text.startswith("+++"):
        return {}
    return tomllib.loads(text.split("+++", 2)[1])


def walkthroughs() -> list[Path]:
    """The folders below content/ that hold a walkthrough."""
    return sorted(p.parent for p in CONTENT.glob("*/_index.md"))


def repo_root(walkthrough: Path) -> Path:
    """The Git repository a walkthrough describes: `source` in its
    _index.md (a path relative to the site folder), else the repository
    the site itself is in."""
    source = front_matter(walkthrough / "_index.md").get("extra", {}).get("source", "..")
    return (SITE / source).resolve()


def commit_of(walkthrough: Path) -> str:
    commit = front_matter(walkthrough / "_index.md").get("extra", {}).get("commit")
    if not commit:
        raise SystemExit(f"{walkthrough.name}/_index.md: no `commit` in [extra]")
    return commit


def source_lines(repo: Path, commit: str, file: str) -> list[str]:
    """The lines of `file` as they are at `commit`."""
    shown = subprocess.run(
        ["git", "-C", str(repo), "show", f"{commit}:{file}"],
        capture_output=True,
        text=True,
    )
    if shown.returncode != 0:
        raise FileNotFoundError(f"{file} at {commit[:7]}: {shown.stderr.strip()}")
    return shown.stdout.split("\n")


@dataclass
class Excerpt:
    path: Path  # the content file
    line: int  # where the excerpt starts in it
    file: str
    first: int
    last: int
    hl: str
    body: list[str]

    def runs(self) -> list[tuple[int, list[str]]]:
        """The excerpt as runs of consecutive lines: (first line number,
        lines)."""
        runs, start, run = [], self.first, []
        for text in self.body:
            gap = GAP.match(text)
            if gap:
                runs.append((start, run))
                start, run = int(gap.group(3)), []
            else:
                run.append(text)
        runs.append((start, run))
        return runs


def excerpts(path: Path) -> list[Excerpt]:
    text = path.read_text(encoding="utf-8")
    found = []
    for m in EXCERPT.finditer(text):
        args = dict(ARG.findall(m.group("args")))
        first, _, last = args.get("lines", "0").partition("-")
        found.append(
            Excerpt(
                path=path,
                line=text.count("\n", 0, m.start()) + 1,
                file=args.get("file", ""),
                first=int(first),
                last=int(last or first),
                hl=args.get("hl", ""),
                body=m.group("body").split("\n"),
            )
        )
    return found


def ranges(spec: str) -> list[tuple[int, int]]:
    """"147-153 156" as [(147, 153), (156, 156)]."""
    out = []
    for part in spec.split():
        a, _, b = part.partition("-")
        out.append((int(a), int(b or a)))
    return out
