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
  if (url) sendToAdm(url, undefined, info.pageUrl);
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
  sendToAdm(url, filename, item.referrer);
});

// Kumpulkan Cookie header untuk URL agar unduhan ber-autentikasi (mis. lampiran
// Gmail) bisa diunduh ADM. Mengembalikan string "k=v; k2=v2" atau "".
async function cookieHeaderFor(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    if (cookies && cookies.length) {
      return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
    }
  } catch (e) {
    /* tak ada izin / gagal — abaikan, unduh tanpa cookie */
  }
  return "";
}

async function sendToAdm(url, filename, referrer) {
  if (isDuplicate(url)) return;
  const msg = { method: "download.add", url, userAgent: navigator.userAgent };
  if (filename) msg.filename = filename;
  if (referrer) msg.referrer = referrer;
  const cookie = await cookieHeaderFor(url);
  if (cookie) msg.cookies = cookie;
  chrome.runtime.sendNativeMessage(HOST, msg, () => {
    if (chrome.runtime.lastError) {
      console.warn("ADM bridge:", chrome.runtime.lastError.message);
    }
  });
}

// ===== Fase 1: deteksi video/audio progresif yang sedang diputar =====
// Pantau respons jaringan; bila bertipe media progresif (mp4/webm/flv/mp3 dst),
// catat per-tab & tampilkan panel "Download with ADM" via content script.
const MEDIA_EXT = /\.(mp4|webm|flv|m4v|mov|mkv|mp3|m4a|aac|ogg|wav|3gp)(\?|$)/i;
const MEDIA_CT = /^(video|audio)\//i;
// Fase 2: stream HLS (.m3u8). DASH (.mpd) belum didukung engine → tak ditawarkan.
const HLS_EXT = /\.m3u8(\?|$)/i;
const HLS_CT = /mpegurl/i;
const MIN_SIZE = 200 * 1024; // lewati klip kecil/iklan

// Nama keluaran untuk stream HLS: basename tanpa query, ekstensi → .ts.
function hlsName(url) {
  try {
    const base = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
    const stem = base.replace(/\.m3u8$/i, "");
    if (stem && !/^(index|master|playlist|chunklist)$/i.test(stem)) return `${stem}.ts`;
  } catch (e) {
    /* abaikan */
  }
  return "video.ts";
}

// tabId -> Map(url -> {url, type, size, filename})
const mediaByTab = new Map();

function headerVal(headers, name) {
  const h = (headers || []).find((x) => x.name.toLowerCase() === name);
  return h ? h.value : undefined;
}

function extFromCt(ct) {
  if (!ct) return null;
  if (ct.includes("mp4")) return "mp4";
  if (ct.includes("webm")) return "webm";
  if (ct.includes("flv")) return "flv";
  if (ct.includes("mpeg")) return "mp3";
  if (ct.includes("3gpp")) return "3gp";
  if (ct.includes("ogg")) return "ogg";
  return null;
}

function guessName(url, ct) {
  try {
    const base = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
    if (base && /\.[a-z0-9]{2,4}$/i.test(base)) return base;
  } catch (e) {
    /* abaikan */
  }
  return `video.${extFromCt(ct) || "mp4"}`;
}

function containerOf(url, ct) {
  const m = MEDIA_EXT.exec(url);
  if (m) return m[1].toUpperCase();
  return (extFromCt(ct) || "MEDIA").toUpperCase();
}

function pushToTab(tabId) {
  const m = mediaByTab.get(tabId);
  const items = m ? [...m.values()] : [];
  chrome.tabs.sendMessage(tabId, { type: "adm-media", items }, () => void chrome.runtime.lastError);
}

function addMedia(tabId, item) {
  let m = mediaByTab.get(tabId);
  if (!m) {
    m = new Map();
    mediaByTab.set(tabId, m);
  }
  if (m.has(item.url)) return;
  m.set(item.url, item);
  pushToTab(tabId);
}

chrome.webRequest.onHeadersReceived.addListener(
  (d) => {
    if (!enabled || d.tabId < 0) return;
    const ct = (headerVal(d.responseHeaders, "content-type") || "").toLowerCase();
    // Fase 2: playlist HLS → tawarkan sebagai stream (engine unduh & gabung segmen).
    // Dedupe per-URL (addMedia) menahan reload playlist live. Ukuran tak diketahui.
    if (HLS_CT.test(ct) || HLS_EXT.test(d.url)) {
      addMedia(d.tabId, { url: d.url, type: "HLS", size: 0, filename: hlsName(d.url) });
      return;
    }
    const isMedia = (MEDIA_CT.test(ct) && !ct.includes("mp2t")) || MEDIA_EXT.test(d.url);
    if (!isMedia) return;
    let size = parseInt(headerVal(d.responseHeaders, "content-length") || "0", 10) || 0;
    const cr = headerVal(d.responseHeaders, "content-range"); // total dari 206
    if (cr) {
      const mm = /\/(\d+)\s*$/.exec(cr);
      if (mm) size = parseInt(mm[1], 10);
    }
    if (size && size < MIN_SIZE) return;
    addMedia(d.tabId, {
      url: d.url,
      type: containerOf(d.url, ct),
      size,
      filename: guessName(d.url, ct),
    });
  },
  { urls: ["<all_urls>"] },
  ["responseHeaders"]
);

// Bersihkan daftar saat tab navigasi/ditutup.
chrome.tabs.onUpdated.addListener((tabId, info) => {
  if (info.status === "loading") mediaByTab.delete(tabId);
});
chrome.tabs.onRemoved.addListener((tabId) => mediaByTab.delete(tabId));

// Pesan dari content script: minta daftar / minta unduh item terpilih.
chrome.runtime.onMessage.addListener((msg, sender) => {
  if (!msg || !sender.tab) return;
  if (msg.type === "adm-get-media") {
    pushToTab(sender.tab.id);
  } else if (msg.type === "adm-download" && msg.url) {
    sendToAdm(msg.url, msg.filename, sender.tab.url);
  }
});
