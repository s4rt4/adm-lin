# ADM Browser Integration (Linux)

Extension MV3 + native messaging host (`com.adm.bridge`). Lihat plan §11.

## Komponen
- `extension/` — WebExtension MV3 (Chrome/Chromium/Edge; Firefox perlu sedikit penyesuaian).
- `adm-bridge` — native messaging host (stdio ↔ Unix socket `$XDG_RUNTIME_DIR/adm.sock` ke `adm-egui`).

## ID ekstensi tetap

`manifest.json` memuat field `key` (kunci publik), sehingga **Extension ID selalu**
`akdapmiioimlpcdapnkmonlgmkgjcobb` berapa kali pun dimuat ulang / di mesin mana pun.
Karena itu host native-messaging bisa didaftarkan untuk ID tetap ini tanpa harus
menyalin ID dari `chrome://extensions` tiap kali.

Kunci privat untuk membuat ID tsb ada di `tools/adm-extension-key.pem` (di-`gitignore`,
hanya diperlukan bila kelak ingin mem-pack `.crx`).

## Cara pasang (development, Chrome/Chromium/Edge)

1. **Build** workspace: `cargo build` → `adm-bridge` & `adm-egui` di `target/debug`.
2. **Daftarkan host** (tulis manifest ke `~/.config/<browser>/NativeMessagingHosts/`):
   ```
   target/debug/adm-bridge register akdapmiioimlpcdapnkmonlgmkgjcobb
   ```
   (Path host = lokasi biner `adm-bridge` saat perintah dijalankan.)
3. **Muat extension**: buka `chrome://extensions` → aktifkan *Developer mode* →
   *Load unpacked* → pilih folder `extension/`. ID-nya akan
   `akdapmiioimlpcdapnkmonlgmkgjcobb`.
4. Selesai. Uji:
   - Klik kanan sebuah link → **Download with ADM**, atau
   - Mulai unduhan apa pun → otomatis dibatalkan di browser & diserahkan ke ADM.
   - Bila `adm-egui` belum jalan, bridge otomatis men-spawn-nya.
   - Toggle "tangkap unduhan otomatis" ada di popup ikon extension.

## Lepas
```
target/debug/adm-bridge unregister
```

## Catatan
- Manifest host ditulis ke `~/.config/{google-chrome,chromium,microsoft-edge}/NativeMessagingHosts/com.adm.bridge.json`
  (Chromium: `allowed_origins`), dan `~/.mozilla/native-messaging-hosts/` bila ID Firefox diberikan (`allowed_extensions`).
- Pesan ke ADM: `{"method":"download.add","url":..,"filename":..,"referrer":..,"userAgent":..,"cookies":..}`.
- Saat instalasi (RPM/Flatpak), path host harus menunjuk biner terpasang; jalankan ulang `register`
  setelah memindah biner.
