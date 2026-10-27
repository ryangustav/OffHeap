// CI helper for zig cross-compilation (aarch64 Linux).
//
// @napi-rs/cli 2.x forwards rustc's linker arguments to `zig cc`, dropping only the
// ones zig is known to reject. Recent rustc versions also pass
// `-Wl,--fix-cortex-a53-843419` for aarch64 Linux, which zig's linker rejects with
// "unsupported linker arg", so this script adds it to the CLI's filter.
const fs = require('fs');
const path = require('path');

const file = path.join(__dirname, '..', 'node_modules', '@napi-rs', 'cli', 'scripts', 'index.js');
const filtered = "arg !== '-Wl,--no-undefined-version'";
const patched = `${filtered} && arg !== '-Wl,--fix-cortex-a53-843419'`;

const source = fs.readFileSync(file, 'utf8');
if (source.includes(patched)) {
  console.log('napi-rs zig linker filter already patched');
} else if (!source.includes(filtered)) {
  console.error(`Could not find the zig linker argument filter in ${file}; @napi-rs/cli changed?`);
  process.exit(1);
} else {
  fs.writeFileSync(file, source.replace(filtered, patched));
  console.log('Patched napi-rs zig linker filter to drop -Wl,--fix-cortex-a53-843419');
}
