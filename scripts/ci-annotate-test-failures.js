// CI helper: turns failed node:test cases from a TAP log into GitHub error annotations,
// so failures from jobs whose logs are hard to reach (Docker/QEMU) show up on the run.
//
// Usage: node scripts/ci-annotate-test-failures.js <tap-log-file>
const fs = require('fs');

const file = process.argv[2];
if (!file || !fs.existsSync(file)) {
  console.log(`No test log at ${file}`);
  process.exit(0);
}

const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/);
const escape = (s) => s.replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A');

for (let i = 0; i < lines.length; i++) {
  const match = /^not ok \d+ - (.*)$/.exec(lines[i]);
  if (!match) continue;
  // The YAML diagnostic block follows until the closing "..." line.
  const details = [];
  for (let j = i + 1; j < lines.length && details.length < 40; j++) {
    if (/^\s*\.\.\.\s*$/.test(lines[j])) break;
    details.push(lines[j]);
  }
  console.log(`::error title=${escape(match[1]).replace(/[:,]/g, ' ')}::${escape(details.join('\n'))}`);
}
