import { mkdtempSync, copyFileSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const root = fileURLToPath(new URL('../', import.meta.url));
const output = mkdtempSync(join(tmpdir(), 'prodex-icons-'));
try {
  // Keep the supplied mark unchanged; the Dock gets its own macOS-style tile.
  const logo = readFileSync(join(root, 'public/brand/hyras-logo.svg'), 'utf8')
    .replace('width="290" height="290"', 'x="202" y="202" width="620" height="620"')
    .replaceAll('fill="black"', 'fill="#ffffff"');
  const stylesheet = readFileSync(join(root, 'src/styles.css'), 'utf8');
  const accent = stylesheet.match(/--accent:\s*(#[0-9a-fA-F]{6})\s*;/)?.[1];
  if (!accent) throw new Error('Could not find the app accent color');
  const source = join(root, 'src-tauri/icons/icon.svg');
  writeFileSync(source, `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1024" height="1024" viewBox="0 0 1024 1024">
  <title>Prodex app icon</title>
  <rect x="72" y="80" width="880" height="880" rx="196" fill="#000000" opacity="0.12"/>
  <rect x="72" y="72" width="880" height="880" rx="196" fill="${accent}" stroke="${accent}" stroke-width="2"/>
  ${logo}
</svg>\n`);
  execFileSync(process.execPath, [
    join(root, 'node_modules/@tauri-apps/cli/tauri.js'), 'icon',
    source, '--output', output,
  ], { cwd: root, stdio: 'inherit' });
  for (const name of ['icon.png', 'icon.icns', '32x32.png', '128x128.png', '128x128@2x.png']) {
    copyFileSync(join(output, name), join(root, 'src-tauri/icons', name));
  }
} finally {
  rmSync(output, { recursive: true, force: true });
}
