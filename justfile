# pithddu-dashboard tasks. Run `just` to list.
set shell := ["bash", "-uc"]

default:
    @just --list

# Build the SimHub plugin (net48) and install the DLL into the SimHub folder.
# Builds on Linux against a Wine SimHub prefix — no Windows/Mono needed. Override
# the SimHub path (or set $SIMHUB_PATH); the csproj copies the DLL in on success.
#   just simhub-plugin
#   just simhub-plugin "/path/to/SimHub"
simhub-plugin sh="":
    #!/usr/bin/env bash
    set -euo pipefail
    SH="{{sh}}"
    SH="${SH#sh=}"   # tolerate `just simhub-plugin sh=...` (just args are positional)
    SH="${SH:-${SIMHUB_PATH:-$HOME/linux-simracing-utils/pfx/drive_c/Program Files (x86)/SimHub}}"
    if [ ! -f "$SH/SimHub.Plugins.dll" ]; then
        echo "SimHub.Plugins.dll not found in: $SH" >&2
        echo "Pass the SimHub folder:  just simhub-plugin \"/path/to/SimHub\"" >&2
        exit 1
    fi
    echo "Building plugin against: $SH"
    dotnet build -c Release -p:SimHubPath="$SH" simhub-plugin/PithDdu.SimHubPlugin.csproj

# Build the in-prefix shared-memory tools (pith-shim.exe + pith-shmbridge.exe) for
# Windows / Proton-Wine. Auto-adds the windows-gnu target; needs mingw-w64 installed
# (e.g. `x86_64-w64-mingw32-gcc`). The crate is outside the host workspace.
#   just shm-tools           -> build the two .exe
#   just shm-tools install   -> build + install (.exe → ~/.local/share/pithddu,
#                               pith-shim-run → ~/.local/bin)
shm-tools action="":
    #!/usr/bin/env bash
    set -euo pipefail
    rustup target list --installed | grep -qx x86_64-pc-windows-gnu \
        || rustup target add x86_64-pc-windows-gnu
    cargo build --release --manifest-path pith-shm-bridge/Cargo.toml \
        --target x86_64-pc-windows-gnu
    out="pith-shm-bridge/target/x86_64-pc-windows-gnu/release"
    echo "Built:"
    echo "  $out/pith-shim.exe"
    echo "  $out/pith-shmbridge.exe"
    act="{{action}}"; act="${act#action=}"   # tolerate `action=install`
    if [ "$act" = "install" ]; then
        data="${XDG_DATA_HOME:-$HOME/.local/share}/pithddu"   # .exe files
        bindir="$HOME/.local/bin"                             # wrapper (on PATH)
        mkdir -p "$data" "$bindir"
        cp "$out/pith-shim.exe" "$out/pith-shmbridge.exe" "$data/"
        install -m755 pith-shm-bridge/pith-shim-run "$bindir/pith-shim-run"
        echo "Installed:"
        echo "  $data/{pith-shim.exe,pith-shmbridge.exe}"
        echo "  $bindir/pith-shim-run   (use 'pith-shim-run %command%' if $bindir is on PATH)"
        case ":$PATH:" in *":$bindir:"*) ;; *) echo "  NOTE: $bindir is not on your PATH — add it." ;; esac
    fi

# Render one PNG per dashboard page into docs/screenshots/ (for the README/docs).
# Needs a display (renders briefly via the winit backend); uses seeded demo data.
screenshots dir="docs/screenshots":
    cargo build -p pith-dashboard
    ./target/debug/pith-dashboard --shots "{{dir}}"

# Show the current crate versions (from Cargo.toml) + the latest release tags.
# Note: `just release` only creates/pushes a git tag — it does NOT edit Cargo.toml.
version:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "crate versions (Cargo.toml):"
    for f in dashboard firmware pith-core pith-ui pith-device pith-flash pith-shm-bridge; do
        [ -f "$f/Cargo.toml" ] || continue
        v=$(grep -m1 '^version' "$f/Cargo.toml" | sed -E 's/version *= *"([^"]+)".*/\1/')
        printf "  %-16s %s\n" "$f" "${v:-?}"
    done
    echo "latest tags:"
    git tag -l --sort=-v:refname 2>/dev/null | head -5 | sed 's/^/  /' || true

# Cut a release for ONE stream: bump that crate's Cargo.toml version, commit, tag,
# and push (CI builds the bins from the tag and publishes the GitHub Release). The
# tag prefix + the crate version stay in sync.
#   just release dashboard          -> bump patch of the latest dashboard-v* tag
#   just release firmware           -> bump patch of the latest firmware-v* tag
#   just release dashboard 1.2.3    -> release that stream exactly at 1.2.3
release stream version="":
    #!/usr/bin/env bash
    set -euo pipefail
    stream="{{stream}}"
    case "$stream" in
        dashboard) manifest="dashboard/Cargo.toml" ;;
        firmware)  manifest="firmware/Cargo.toml" ;;
        *) echo "usage: just release <dashboard|firmware> [version]" >&2; exit 1 ;;
    esac
    git fetch --tags --quiet
    ver="{{version}}"
    if [ -z "$ver" ]; then
        last=$(git tag -l "${stream}-v*" --sort=-v:refname | head -n1)
        if [ -n "$last" ]; then
            base=${last#${stream}-v}                 # bump the latest stream tag
        else
            base=$(grep -m1 '^version' "$manifest" | sed -E 's/version *= *"([^"]+)".*/\1/')
        fi                                            # no tag yet → bump Cargo.toml
        IFS='.' read -r MA MI PA <<<"$base"
        ver="${MA:-0}.${MI:-0}.$(( ${PA:-0} + 1 ))"
    fi
    ver=${ver#v}
    tag="${stream}-v${ver}"
    if git rev-parse "$tag" >/dev/null 2>&1; then
        echo "tag $tag already exists — pick another version" >&2
        exit 1
    fi
    # Bump the crate's [package] version (the first version line in its Cargo.toml).
    sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"${ver}\"/" "$manifest"
    branch=$(git rev-parse --abbrev-ref HEAD)
    echo "Releasing $tag from $branch @ $(git rev-parse --short HEAD)"
    git commit -m "release: ${stream} v${ver}" -- "$manifest"
    git tag -a "$tag" -m "release ${tag}"
    git push origin "$branch" "$tag"
    echo "Pushed $branch + $tag — GitHub Actions will build and publish the release."
    if [ "$stream" = "dashboard" ]; then
        echo "When CI has published the release, run:  just aur-publish"
    fi

# Publish the AUR package(s) for the current dashboard version. The version is
# read from dashboard/Cargo.toml (the single source of truth — `just release
# dashboard` bumps it). The in-repo ./aur/ PKGBUILDs are the source of truth: for
# each package this bumps pkgver in ./aur, runs updpkgsums (downloads the release
# assets + writes real sha256sums), regenerates .SRCINFO, copies PKGBUILD +
# .SRCINFO into the ../aur/<pkg> AUR clone, and commits + pushes it. Run after
# `just release dashboard`, once CI has published the assets.
aur-publish:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(grep -m1 '^version' dashboard/Cargo.toml | sed -E 's/version *= *"([^"]+)".*/\1/')
    ver=${ver#v}
    if [ -z "$ver" ]; then echo "could not read version from dashboard/Cargo.toml" >&2; exit 1; fi
    base="https://github.com/lemonxah/pithddu/releases/download/dashboard-v${ver}"
    echo "AUR publish: dashboard ${ver}"

    # Wait for CI to publish the release assets that updpkgsums will hash.
    for a in pith-dashboard-linux-x86_64.tar.gz pith-shm-tools-win64.zip; do
        echo "  waiting for ${a}…"
        for i in $(seq 1 60); do
            curl -fsI "${base}/${a}" >/dev/null 2>&1 && break
            [ "$i" = 60 ] && { echo "    not published after 10m: ${base}/${a}" >&2; exit 1; }
            sleep 10
        done
    done

    for pkg in pithddu-dashboard-bin pithddu-dashboard; do
        src="aur/$pkg"          # in-repo source of truth
        dst="../aur/$pkg"       # AUR clone we push from
        [ -f "$src/PKGBUILD" ] || { echo "  (skip $pkg — no in-repo PKGBUILD)"; continue; }
        if [ ! -d "$dst/.git" ]; then echo "  (skip $pkg — no AUR clone at $dst)"; continue; fi
        echo "  $pkg: bump → ${ver}, updpkgsums, .SRCINFO"
        sed -i -E "s/^pkgver=.*/pkgver=${ver}/;s/^pkgrel=.*/pkgrel=1/" "$src/PKGBUILD"
        # updpkgsums fills sha256sums from the real sources; SKIP stays for git/VCS.
        if ! ( cd "$src" && updpkgsums && makepkg --printsrcinfo > .SRCINFO ); then
            echo "    WARN: $pkg updpkgsums/.SRCINFO failed (source/tag/asset missing?) — skipping" >&2
            continue
        fi
        cp "$src/PKGBUILD" "$src/.SRCINFO" "$dst/"
        ( cd "$dst"
          git add PKGBUILD .SRCINFO
          # commit only if something changed (re-run safe), then push to AUR's
          # required 'master' branch regardless of the local branch name.
          git diff --cached --quiet || git commit -m "$pkg ${ver}"
          git push origin HEAD:master )
        echo "  $pkg ${ver} pushed to AUR"
    done
    echo "Done. Commit the updated aur/ PKGBUILDs (+ .SRCINFO) to the monorepo too."
