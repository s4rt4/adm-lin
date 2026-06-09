// Service worker (MV3): tangkap unduhan → kirim ke ADM via native messaging.
// Plan §11.1. Host: com.adm.bridge.

const HOST = "com.adm.bridge";

// Anti-duplikat: lewati URL yang sama yang baru dikirim < 5 detik lalu
// (onCreated bisa terpicu lebih dari sekali untuk satu unduhan).
const recentlySent = new Map();
function isDuplicate(url) {
  const now = Date.now();
  for (const [u, t] of recentlySent) if (now - t > 5000) recentlySent.delete(u);
  if (recentlySent.has(url) && now - recentlySent.get(url) < 5000) return true;
  recentlySent.set(url, now);
  return false;
}

let enabled = true;
chrome.storage.local.get({ enabled: true }, (v) => { enabled = v.enabled; });
chrome.storage.onChanged.addListener((changes) => {
  if (changes.enabled) enabled = changes.enabled.newValue;
});

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "adm-download",
    title: "Download with ADM",
    contexts: ["link"],
  });
});

// Klik kanan pada link → kirim ke ADM (selalu, walau toggle off).
chrome.contextMenus.onClicked.addListener((info) => {
  const url = info.linkUrl || info.srcUrl;
  if (url) sendToAdm(url);
});

// Tangkap unduhan baru: batalkan di browser, serahkan ke ADM.
chrome.downloads.onCreated.addListener(async (item) => {
  if (!enabled) return;
  const url = item.finalUrl || item.url;
  if (!url || !/^https?:/i.test(url)) return;
  try {
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
  } catch (e) {
    /* sudah selesai/tak bisa dibatalkan — abaikan */
  }
  const filename = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  sendToAdm(url, filename);
});

function sendToAdm(url, filename) {
  if (isDuplicate(url)) return;
  const msg = { method: "download.add", url };
  if (filename) msg.filename = filename;
  chrome.runtime.sendNativeMessage(HOST, msg, () => {
    if (chrome.runtime.lastError) {
      console.warn("ADM bridge:", chrome.runtime.lastError.message);
    }
  });
}
