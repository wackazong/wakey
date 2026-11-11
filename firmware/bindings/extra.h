// Extra ESP-IDF headers for bindgen, beyond esp-idf-sys's default set.
// Bindings land in the esp_idf_sys crate root.
#include "driver/ppa.h"
// The camera as a light meter (src/ambient.rs): MIPI-CSI receiver and ISP.
#include "esp_cam_ctlr_csi.h"
#include "esp_cam_ctlr.h"
#include "driver/isp.h"
// esp_hosted's transport init/deinit (src/net.rs: Wi-Fi off between syncs).
#include "esp_hosted.h"
