const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const chromePath = 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe';
const projectRoot = path.resolve(__dirname, '..');
const htmlPath = 'file:///' + path.join(projectRoot, 'settings.html').replace(/\\/g, '/');

// Audit runner for Chrome headless
const testHtml = `<!DOCTYPE html>
<html>
<body>
<iframe id="f" src="${htmlPath}" width="1000" height="625" style="border:none;"></iframe>
<div id="results"></div>
<script>
window.addEventListener('message', (e) => {});
const iframe = document.getElementById('f');
iframe.onload = async () => {
  const win = iframe.contentWindow;
  const doc = iframe.contentDocument;

  try {
    await doc.fonts.ready;
    win.showTab('about');

    const lightResult = win.runAboutAlignmentAuditSingleTheme('light');
    const darkResult = win.runAboutAlignmentAuditSingleTheme('dark');

    const out = {
      light: lightResult,
      dark: darkResult
    };
    document.getElementById('results').textContent = JSON.stringify(out, null, 2);
  } catch (err) {
    document.getElementById('results').textContent = JSON.stringify({ error: String(err), stack: err.stack });
  }
};
</script>
</body>
</html>`;

const tempHtmlPath = path.join(projectRoot, 'temp_test_runner.html');
fs.writeFileSync(tempHtmlPath, testHtml);

try {
  const stdout = execFileSync(chromePath, [
    '--headless=new',
    '--window-size=1000,625',
    '--virtual-time-budget=5000',
    '--dump-dom',
    '--allow-file-access-from-files',
    'file:///' + tempHtmlPath.replace(/\\/g, '/')
  ], { maxBuffer: 10 * 1024 * 1024 });

  const str = stdout.toString();
  const match = str.match(/<div id="results">([\s\S]*?)<\/div>/);
  if (match) {
    const raw = match[1].replace(/&quot;/g, '"').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');
    console.log(raw);
  } else {
    console.log('No results div matched. HTML was:\n', str.slice(0, 1000));
  }
} catch (e) {
  console.error(e);
} finally {
  if (fs.existsSync(tempHtmlPath)) {
    fs.unlinkSync(tempHtmlPath);
  }
}
