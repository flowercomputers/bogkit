// Loads the compiled napi binary. Build it with `npm run build:addon`
// (cargo build -p sod-web-addon --release + copy into this directory).
module.exports = require("./sod_web_addon.node");
