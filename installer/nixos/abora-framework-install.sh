# abora-framework-install: install a NixOS system built on Abora Framework.
#
# It partitions ONE disk (erasing it), writes a small NixOS flake that imports the Framework, and runs
# nixos-install. You choose whether to import ANIX (from Abora OS) and TinyPM. Run it from the Abora
# Framework installer ISO, as root.
#
# This file is the body of a Nix `writeShellApplication` (see nix/installer.nix): the bash shebang and
# `set -euo pipefail` are added there, and shellcheck runs over it when the package is built.
#
# `--generate-only DIR` writes the flake and configuration into DIR and stops: no disk is touched, no root
# is needed. The tests use it, and it is a safe way to see exactly what would be installed.

VERSION="0.1.0"
DEFAULT_FLAKE="github:AboraOS-Project/Abora-Framework"

disk=""
hostname="abora-framework"
username="admin"
timezone="UTC"
password_hash=""
want_anix=""      # "" = ask, yes, no
want_tinypm=""    # "" = ask, yes, no
framework_flake="$DEFAULT_FLAKE"
generate_only=""
boot_mode=""      # uefi or bios; detected unless given (only --generate-only needs to say)
assume_yes=0
dry_run=0
state_version=""

usage() {
    cat <<USAGE
abora-framework-install $VERSION

Usage: abora-framework-install [options]

  --disk DEVICE           disk to install to. EVERYTHING ON IT IS ERASED
  --hostname NAME         machine name (default: $hostname)
  --user NAME             administrator account (default: $username)
  --password-hash HASH    that account's password hash (mkpasswd -m sha-512). Asked for if omitted
  --timezone ZONE         for example Europe/Berlin (default: $timezone)
  --anix / --no-anix      import ANIX from Abora OS, or not (asked if omitted)
  --tinypm / --no-tinypm  import TinyPM (grab, tinypm), or not (asked if omitted)
  --framework-flake REF   where the Framework comes from (default: $DEFAULT_FLAKE)
  --generate-only DIR     write the flake and configuration into DIR and stop; nothing else is touched
  --boot-mode MODE        uefi or bios (default: detected; --generate-only defaults to uefi)
  --dry-run               show what would be done and stop
  --yes                   do not ask for the final confirmation (needs --disk)
  -h, --help              this help
USAGE
}

die() { echo "abora-framework-install: $*" >&2; exit 1; }
say() { echo ">> $*"; }
is_tty() { [ -t 0 ] && [ -t 1 ]; }

while [ $# -gt 0 ]; do
    case "$1" in
        --disk) disk="${2:?--disk needs a device}"; shift ;;
        --hostname) hostname="${2:?--hostname needs a name}"; shift ;;
        --user) username="${2:?--user needs a name}"; shift ;;
        --password-hash) password_hash="${2:?--password-hash needs a hash}"; shift ;;
        --timezone) timezone="${2:?--timezone needs a zone}"; shift ;;
        --anix) want_anix=yes ;;
        --no-anix) want_anix=no ;;
        --tinypm) want_tinypm=yes ;;
        --no-tinypm) want_tinypm=no ;;
        --framework-flake) framework_flake="${2:?--framework-flake needs a flake reference}"; shift ;;
        --generate-only) generate_only="${2:?--generate-only needs a directory}"; shift ;;
        --boot-mode) boot_mode="${2:?--boot-mode needs uefi or bios}"; shift ;;
        --state-version) state_version="${2:?--state-version needs a release}"; shift ;;
        --dry-run) dry_run=1 ;;
        --yes|-y) assume_yes=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option '$1'" ;;
    esac
    shift
done

# --- validation of everything that ends up inside the generated Nix files ----------------------------
# These values are written into Nix source, so they are restricted to characters that cannot break out of
# a string. Anything else is refused, not escaped.

valid_hostname() { [[ "$1" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]]; }
valid_username() { [[ "$1" =~ ^[a-z_][a-z0-9_-]{0,30}$ ]] && [ "$1" != "root" ]; }
valid_timezone() { [[ "$1" =~ ^[A-Za-z0-9_+/-]{1,64}$ ]] && [[ "$1" != *..* ]]; }
valid_hash() { [[ "$1" =~ ^\$[0-9a-z]+\$[A-Za-z0-9./,=\$-]+$ ]]; }
valid_flake_ref() { [[ "$1" =~ ^[A-Za-z0-9_.:/@?=+~%-]+$ ]]; }
valid_release() { [[ "$1" =~ ^[0-9]{2}\.[0-9]{2}$ ]]; }

check_settings() {
    valid_hostname "$hostname" || die "invalid hostname '$hostname' (lowercase letters, digits and dashes)"
    valid_username "$username" || die "invalid user name '$username' (lowercase letters, digits, - and _; not root)"
    valid_timezone "$timezone" || die "invalid time zone '$timezone'"
    valid_flake_ref "$framework_flake" || die "invalid flake reference '$framework_flake'"
    if [ -n "$password_hash" ]; then
        valid_hash "$password_hash" || die "that does not look like a password hash (expected something like \$6\$salt\$hash)"
    fi
    if [ -n "$boot_mode" ]; then
        case "$boot_mode" in uefi|bios) ;; *) die "--boot-mode must be uefi or bios" ;; esac
    fi
    if [ -n "$state_version" ]; then
        valid_release "$state_version" || die "invalid --state-version '$state_version' (expected like 26.05)"
    fi
}

# --- generating the system flake ------------------------------------------------------------------------

# Write flake.nix and configuration.nix into $1.
write_system_config() {
    local dir="$1" mode="$2" target_disk="$3" release="$4"
    mkdir -p "$dir"

    local anix_module="" tinypm_module=""
    if [ "$want_anix" = yes ]; then
        anix_module="        abora-framework.nixosModules.anix"$'\n'
    fi
    if [ "$want_tinypm" = yes ]; then
        tinypm_module="        abora-framework.nixosModules.tinypm"$'\n'
    fi

    cat > "$dir/flake.nix" <<FLAKE
{
  description = "$hostname (built on Abora Framework)";

  inputs = {
    abora-framework.url = "$framework_flake";
    nixpkgs.follows = "abora-framework/nixpkgs";
  };

  outputs =
    { nixpkgs, abora-framework, ... }:
    {
      nixosConfigurations."$hostname" = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ./hardware-configuration.nix
          ./configuration.nix
          abora-framework.nixosModules.framework
${anix_module}${tinypm_module}        ];
      };
    };
}
FLAKE

    local bootloader
    if [ "$mode" = uefi ]; then
        bootloader='  boot.loader.systemd-boot.enable = true;
  boot.loader.efi.canTouchEfiVariables = true;'
    else
        bootloader="  boot.loader.grub.enable = true;
  boot.loader.grub.device = \"$target_disk\";"
    fi

    local password_line=""
    if [ -n "$password_hash" ]; then
        password_line="    hashedPassword = \"$password_hash\";"
    fi

    cat > "$dir/configuration.nix" <<CONFIG
# Your system. This file and flake.nix are yours to edit; rebuild with
#   sudo nixos-rebuild switch --flake /etc/nixos#$hostname
{ ... }:

{
  networking.hostName = "$hostname";
  time.timeZone = "$timezone";

  # The Abora Framework base: the aborad daemon and the abora command.
  aboraFramework.enable = true;

  users.users."$username" = {
    isNormalUser = true;
    extraGroups = [ "wheel" ];
$password_line
  };

$bootloader

  system.stateVersion = "$release";
}
CONFIG
}

# --- interactive questions --------------------------------------------------------------------------------

ask() { # prompt, default -> answer on stdout
    local answer
    read -r -p "$1 [$2] " answer
    echo "${answer:-$2}"
}

ask_yes_no() { # prompt, default (y or n) -> yes/no
    local answer default="$2"
    while true; do
        read -r -p "$1 [$( [ "$default" = y ] && echo Y/n || echo y/N )] " answer
        answer="${answer:-$default}"
        case "$answer" in
            y|Y|yes|YES) echo yes; return ;;
            n|N|no|NO) echo no; return ;;
        esac
        echo "Please answer y or n."
    done
}

choose_disk() {
    echo
    echo "Disks:"
    lsblk -dno PATH,SIZE,MODEL,TYPE | grep -E ' disk$' | sed 's/^/  /' || true
    echo
    read -r -p "Install to which disk (for example /dev/sda)? EVERYTHING ON IT WILL BE ERASED: " disk
}

ask_password() {
    local first second
    while true; do
        read -r -s -p "Password for $username: " first; echo
        [ -n "$first" ] || { echo "The password cannot be empty."; continue; }
        read -r -s -p "Repeat it: " second; echo
        if [ "$first" = "$second" ]; then
            password_hash="$(printf '%s' "$first" | mkpasswd -m sha-512 --stdin)"
            return
        fi
        echo "They do not match, try again."
    done
}

# --- main ------------------------------------------------------------------------------------------------

check_settings

if [ -z "$generate_only" ] && [ "$dry_run" = 0 ] && [ "$(id -u)" != 0 ]; then
    die "run as root (sudo), or use --generate-only DIR to only write the configuration"
fi

if is_tty && [ "$assume_yes" = 0 ]; then
    echo "Abora Framework installer $VERSION"
    if [ -z "$generate_only" ] && [ -z "$disk" ]; then choose_disk; fi
    hostname="$(ask 'Hostname' "$hostname")"
    username="$(ask 'Administrator user name' "$username")"
    timezone="$(ask 'Time zone' "$timezone")"
    if [ -z "$want_anix" ]; then
        want_anix="$(ask_yes_no 'Import ANIX (Abora OS'"'"'s system management tool, includes TinyPM)?' n)"
    fi
    if [ -z "$want_tinypm" ]; then
        want_tinypm="$(ask_yes_no 'Import TinyPM (grab and tinypm package tools)?' n)"
    fi
    check_settings
    if [ -z "$password_hash" ] && [ -z "$generate_only" ] && [ "$dry_run" = 0 ]; then ask_password; fi
fi

# Not interactive and not told: leave optional parts out (the safe, minimal answer).
: "${want_anix:=no}"
: "${want_tinypm:=no}"

if [ -z "$boot_mode" ]; then
    if [ -n "$generate_only" ]; then
        boot_mode=uefi
    elif [ -d /sys/firmware/efi ]; then
        boot_mode=uefi
    else
        boot_mode=bios
    fi
fi

# --- generate-only: write and stop -----------------------------------------------------------------------
if [ -n "$generate_only" ]; then
    release="${state_version:-26.05}"
    write_system_config "$generate_only" "$boot_mode" "${disk:-/dev/sda}" "$release"
    # A stand-in so the flake can be evaluated without a machine; the real one comes from nixos-generate-config.
    echo '{ ... }: { fileSystems."/" = { device = "/dev/disk/by-label/abora-root"; fsType = "ext4"; }; }' \
        > "$generate_only/hardware-configuration.nix"
    say "wrote $generate_only/flake.nix, configuration.nix and a placeholder hardware-configuration.nix"
    say "ANIX: $want_anix, TinyPM: $want_tinypm, boot mode: $boot_mode"
    exit 0
fi

# --- a real install ----------------------------------------------------------------------------------------
[ -n "$disk" ] || die "no disk given: pass --disk DEVICE"
[ -b "$disk" ] || die "$disk is not a block device"
if lsblk -no MOUNTPOINTS "$disk" | grep -q '[^[:space:]]'; then
    die "$disk has mounted filesystems; unmount them first (or you picked the disk you booted from)"
fi
if [ -z "$password_hash" ]; then
    if [ "$dry_run" = 1 ]; then
        # shellcheck disable=SC2016 # a literal hash, not an expansion
        password_hash='$6$dry$run'
    else
        die "no password: pass --password-hash HASH, or run interactively"
    fi
fi

if [ -n "$state_version" ]; then
    release="$state_version"
elif command -v nixos-version >/dev/null 2>&1; then
    release="$(nixos-version --release | cut -c1-5)"
else
    release="26.05"
fi
valid_release "$release" || die "could not work out a NixOS release for system.stateVersion (got '$release'); pass --state-version"

echo
echo "About to install:"
echo "  disk:        $disk (ERASED)"
echo "  boot mode:   $boot_mode"
echo "  hostname:    $hostname"
echo "  user:        $username"
echo "  time zone:   $timezone"
echo "  Framework:   $framework_flake"
echo "  ANIX:        $want_anix"
echo "  TinyPM:      $want_tinypm"
echo

if [ "$dry_run" = 1 ]; then
    say "dry run: nothing was changed. It would partition $disk ($boot_mode), format it, write the configuration and run nixos-install."
    exit 0
fi

if [ "$assume_yes" = 0 ]; then
    is_tty || die "not a terminal: pass --yes to confirm the erase non-interactively"
    read -r -p "Type the disk path ($disk) to confirm that it will be erased: " confirm
    [ "$confirm" = "$disk" ] || die "confirmation did not match; nothing was changed"
fi

say "partitioning $disk"
wipefs -a "$disk" >/dev/null
parted -s "$disk" mklabel gpt
if [ "$boot_mode" = uefi ]; then
    parted -s "$disk" mkpart abora-esp fat32 1MiB 513MiB
    parted -s "$disk" set 1 esp on
    parted -s "$disk" mkpart abora-root ext4 513MiB 100%
else
    parted -s "$disk" mkpart abora-bios 1MiB 2MiB
    parted -s "$disk" set 1 bios_grub on
    parted -s "$disk" mkpart abora-root ext4 2MiB 100%
fi
udevadm settle
partprobe "$disk" || true
udevadm settle

root_part=/dev/disk/by-partlabel/abora-root
for _ in $(seq 1 20); do [ -e "$root_part" ] && break; sleep 0.5; done
[ -e "$root_part" ] || die "the new root partition did not appear"

say "formatting"
mkfs.ext4 -q -F -L abora-root "$root_part"
mount /dev/disk/by-label/abora-root /mnt
if [ "$boot_mode" = uefi ]; then
    esp_part=/dev/disk/by-partlabel/abora-esp
    mkfs.fat -F 32 -n ABORA-BOOT "$esp_part" >/dev/null
    mkdir -p /mnt/boot
    mount /dev/disk/by-label/ABORA-BOOT /mnt/boot
fi

say "writing the system configuration to /mnt/etc/nixos"
nixos-generate-config --root /mnt >/dev/null
write_system_config /mnt/etc/nixos "$boot_mode" "$disk" "$release"

say "locking the flake (this downloads the Framework)"
nix flake lock /mnt/etc/nixos

say "installing (this downloads and builds the system; it takes a while)"
nixos-install --no-root-passwd --flake "/mnt/etc/nixos#$hostname"

say "done. Remove the installation medium and reboot."
