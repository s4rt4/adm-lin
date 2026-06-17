# RPM spec ADM (Fedora). Build offline-vendored disarankan:
#   cargo vendor vendor/ && tar czf adm-0.1.0.tar.gz ...   (atau pakai %cargo_* macros)
# Spec ini memakai macro rust-packaging Fedora (rust2rpm). Untuk build cepat
# lokal lihat packaging/linux/install.sh.
Name:           adm
Version:        0.1.0
Release:        1%{?dist}
Summary:        Alpha Download Manager — multi-connection download manager (IDM-style)

License:        MIT
URL:            https://github.com/s4rt4/adm-lin
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
# eframe/egui (winit) butuh pustaka dev sistem:
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gtk+-3.0)
BuildRequires:  libxkbcommon-devel
BuildRequires:  wayland-devel
BuildRequires:  desktop-file-utils

# Font statik untuk UI (egui hang pada variable font):
Requires:       liberation-sans-fonts
Recommends:     xdg-utils

%description
ADM adalah download manager bergaya IDM: multi-koneksi, resume, kategori,
antrian, dan integrasi browser via native messaging. Port Linux memakai egui.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release -p adm-egui -p adm-bridge

%install
install -Dm755 target/release/adm-egui  %{buildroot}%{_bindir}/adm-egui
install -Dm755 target/release/adm-bridge %{buildroot}%{_bindir}/adm-bridge
install -Dm644 packaging/linux/adm-egui.desktop \
    %{buildroot}%{_datadir}/applications/adm-egui.desktop
install -Dm644 crates/adm-egui/assets/logo.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/adm.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/adm-egui.desktop

%files
%license LICENSE
%doc README.md
%{_bindir}/adm-egui
%{_bindir}/adm-bridge
%{_datadir}/applications/adm-egui.desktop
%{_datadir}/icons/hicolor/scalable/apps/adm.svg

%changelog
* Wed Jun 18 2026 s4rt4 <surat.sarta@gmail.com> - 0.1.0-1
- Rilis awal port Linux (egui): persist, IPC Unix socket, bridge, tray, dark mode.
