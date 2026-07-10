const fs = require('fs');
const path = require('path');

// Skip verification if we are running in the development repository itself
if (fs.existsSync(path.join(__dirname, '..', 'Cargo.toml'))) {
  process.exit(0);
}

try {
  // Attempt to load the native binary via the binding loader
  require('../binding.js');
} catch (e) {
  console.error(
    `\n❌ OffHeap Error: Native binary loader 'binding.js' failed to load.\n` +
    `This package depends on a platform-specific native binary compiled in Rust.\n` +
    `It looks like the platform package for your environment (${process.platform}-${process.arch}) failed to install.\n` +
    `Please verify internet access or report this issue: https://github.com/ryangustav/OffHeap/issues\n`
  );
  process.exit(1);
}
