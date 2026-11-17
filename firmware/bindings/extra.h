// Extra ESP-IDF headers for bindgen, beyond esp-idf-sys's default set.
// Bindings land in the esp_idf_sys crate root.
// The PPA rotates rendered strips into the frame buffer (src/display/ppa.rs).
#include "driver/ppa.h"
// The camera as a light meter (src/ambient.rs): MIPI-CSI receiver and ISP.
#include "esp_cam_ctlr_csi.h"
#include "esp_cam_ctlr.h"
#include "driver/isp.h"
// esp_hosted's transport, slave OTA and peer data (src/c6/).
#include "esp_hosted.h"
