// Toggle "tangkap unduhan otomatis" (disimpan di chrome.storage).
const box = document.getElementById("enabled");

chrome.storage.local.get({ enabled: true }, (v) => {
  box.checked = v.enabled;
});

box.addEventListener("change", () => {
  chrome.storage.local.set({ enabled: box.checked });
});
