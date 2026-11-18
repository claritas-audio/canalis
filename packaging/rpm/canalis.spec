# COPR build of Canalis from the release tag (docs/DEVELOPMENT.md › Releases). Same layout
# as packaging/nfpm.yaml: binaries in /usr/lib/canalis/bin so <bin>/../lib/clap finds the
# Claritas CLAPs (ADR 0007). Cargo fetches crates at build time: the COPR project has
# networking on. .copr/Makefile sets Version from Cargo.toml.
%global claritas_commit 1ae0a5b8ec2ecd86439b3de5b73935d559f043cf
%global debug_package %{nil}

Name:           canalis
Version:        0.2.0
Release:        1%{?dist}
Summary:        PipeWire router with effect chains
License:        GPL-3.0-or-later
URL:            https://github.com/claritas-audio/canalis
Source0:        %{url}/archive/v%{version}/canalis-%{version}.tar.gz
# The Claritas EQ and Comp CLAPs; keep the commit in step with scripts/bundle-claritas.sh.
Source1:        https://github.com/claritas-audio/claritas/archive/%{claritas_commit}/claritas-%{claritas_commit}.tar.gz
ExclusiveArch:  x86_64

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
BuildRequires:  clang-devel
BuildRequires:  pkgconf-pkg-config
BuildRequires:  pipewire-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  libX11-devel
BuildRequires:  libxcb-devel
BuildRequires:  xcb-util-wm-devel
BuildRequires:  libXcursor-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  libxkbcommon-x11-devel
BuildRequires:  wayland-devel
BuildRequires:  desktop-file-utils
BuildRequires:  appstream

# winit and glutin open these at run time, so rpm does not find them in the binaries.
Requires:       pipewire-libs
Requires:       libglvnd-egl
Requires:       libglvnd-glx
Requires:       libwayland-client
Requires:       libwayland-cursor
Requires:       libwayland-egl
Requires:       libxkbcommon
Requires:       libxkbcommon-x11
Requires:       libX11
Requires:       libX11-xcb
Requires:       libXcursor
Requires:       libXi
# Opened at run time too: without them LV2 is off, the rest works.
Recommends:     lilv-libs
Recommends:     suil

%description
Virtual microphones and outputs, per-app routing, a patchbay and effect chains
with the Claritas EQ and Comp plus CLAP and LV2 plugins, for the PipeWire desktop.

%prep
# Side by side, not nested: nice-plug's xtask builds in the outermost folder with a Cargo.toml.
%setup -q -c -n canalis-build -a 1

%build
cd canalis-%{version}
cargo build --release --locked -p canalis -p canalis-chain
cd ../claritas-%{claritas_commit}
cargo xtask bundle claritas-eq --release
cargo xtask bundle claritas-comp --release

%install
cd canalis-%{version}
app=io.github.claritas_audio.Canalis
install -Dm755 -t %{buildroot}%{_prefix}/lib/canalis/bin target/release/canalis target/release/canalis-chain
install -Dm755 -t %{buildroot}%{_prefix}/lib/canalis/lib/clap ../claritas-%{claritas_commit}/target/bundled/*.clap
install -d %{buildroot}%{_bindir}
ln -s ../lib/canalis/bin/canalis %{buildroot}%{_bindir}/canalis
install -Dm644 packaging/flatpak/$app.desktop %{buildroot}%{_datadir}/applications/$app.desktop
install -Dm644 packaging/flatpak/$app.metainfo.xml %{buildroot}%{_datadir}/metainfo/$app.metainfo.xml
install -Dm644 assets/canalis-app-icon.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/$app.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/*.desktop
appstreamcli validate --no-net %{buildroot}%{_datadir}/metainfo/*.metainfo.xml
%{buildroot}%{_prefix}/lib/canalis/bin/canalis --version
%{buildroot}%{_prefix}/lib/canalis/bin/canalis-chain --scan "%{buildroot}%{_prefix}/lib/canalis/lib/clap/Claritas EQ.clap" | grep -q audio.claritas.eq

%files
%license canalis-%{version}/LICENSE
%doc canalis-%{version}/README.md
%{_bindir}/canalis
%{_prefix}/lib/canalis/
%{_datadir}/applications/io.github.claritas_audio.Canalis.desktop
%{_datadir}/metainfo/io.github.claritas_audio.Canalis.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.github.claritas_audio.Canalis.svg

%changelog
* Mon Nov 16 2026 Claritas Audio <noreply@github.com> - 0.2.0-1
- First COPR build; release notes on GitHub Releases.
