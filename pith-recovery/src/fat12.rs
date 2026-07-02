//! Minimal FAT12 image builder for the recovery "Mount as USB drive" option.
//!
//! Builds a small read-only disk image in RAM containing a handful of files
//! (the device's saved NVS config blobs), which pith_msc then serves over USB
//! mass storage. FAT12 because the disk is tiny (a 1 MiB volume with 4 KiB
//! clusters is ~250 clusters — far under FAT12's 4085-cluster ceiling), and
//! every OS mounts it without drivers.
//!
//! Geometry: 512-byte sectors, 8 sectors/cluster (4 KiB), 1 FAT, 64 root
//! entries. Layout: [boot | FAT ×1 | root dir ×4 | data]. Files are allocated
//! contiguously — chains are strictly sequential runs ending in EOC.

const SECTOR: usize = 512;
const SEC_PER_CLUSTER: usize = 8;
const CLUSTER: usize = SECTOR * SEC_PER_CLUSTER;
const ROOT_ENTRIES: usize = 64; // 64 × 32 B = 4 sectors
const ROOT_SECTORS: usize = ROOT_ENTRIES * 32 / SECTOR;
const FAT_SECTORS: usize = 1; // 1 sector = 341 FAT12 entries ≥ our cluster count
const TOTAL_SECTORS: usize = 2048; // 1 MiB volume
const DATA_START_SECTOR: usize = 1 + FAT_SECTORS + ROOT_SECTORS;

/// One file to place in the root directory: (8.3 name, contents).
pub struct FileEntry<'a> {
    /// Display name — anything; it's converted to a padded 8.3 (invalid chars
    /// become `_`, name/ext truncated to 8/3). E.g. `"ui.json"` → `UI.JSO`.
    pub name: &'a str,
    pub data: &'a [u8],
}

/// Build the FAT12 disk image. Files that don't fit are silently truncated out
/// (the volume is sized generously above every real blob combination). Returns
/// a full `TOTAL_SECTORS × 512` image, ready for `pith_msc_start`.
pub fn build(files: &[FileEntry]) -> Vec<u8> {
    let mut disk = vec![0u8; TOTAL_SECTORS * SECTOR];

    // ---- boot sector / BPB ----
    {
        let b = &mut disk[..SECTOR];
        b[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]); // jmp short + nop
        b[3..11].copy_from_slice(b"PITHDDU ");
        b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        b[13] = SEC_PER_CLUSTER as u8;
        b[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved sectors
        b[16] = 1; // number of FATs
        b[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
        b[19..21].copy_from_slice(&(TOTAL_SECTORS as u16).to_le_bytes());
        b[21] = 0xF8; // media descriptor: fixed disk
        b[22..24].copy_from_slice(&(FAT_SECTORS as u16).to_le_bytes());
        b[24..26].copy_from_slice(&32u16.to_le_bytes()); // sectors/track (cosmetic)
        b[26..28].copy_from_slice(&2u16.to_le_bytes()); // heads (cosmetic)
        b[36] = 0x80; // drive number
        b[38] = 0x29; // extended boot signature
        b[39..43].copy_from_slice(&0x50495448u32.to_le_bytes()); // volume id "PITH"
        b[43..54].copy_from_slice(b"PITH NVS   ");
        b[54..62].copy_from_slice(b"FAT12   ");
        b[510] = 0x55;
        b[511] = 0xAA;
    }

    // ---- FAT (entries 0/1 reserved) ----
    fat_set(&mut disk, 0, 0xFF8);
    fat_set(&mut disk, 1, 0xFFF);

    // ---- volume label as root entry 0 ----
    {
        let e = root_entry_mut(&mut disk, 0);
        e[0..11].copy_from_slice(b"PITH NVS   ");
        e[11] = 0x08; // ATTR_VOLUME_ID
    }

    // ---- files: contiguous clusters starting at 2 ----
    let max_clusters = (TOTAL_SECTORS - DATA_START_SECTOR) / SEC_PER_CLUSTER;
    let mut next_cluster = 2usize;
    let mut next_entry = 1usize;
    for f in files {
        if f.data.is_empty() || next_entry >= ROOT_ENTRIES {
            continue;
        }
        let clusters = f.data.len().div_ceil(CLUSTER);
        if next_cluster + clusters > 2 + max_clusters {
            continue; // doesn't fit — skip rather than corrupt the image
        }
        // chain + data
        for k in 0..clusters {
            let c = next_cluster + k;
            let next = if k + 1 == clusters { 0xFFF } else { (c + 1) as u16 };
            fat_set(&mut disk, c, next);
            let src_off = k * CLUSTER;
            let n = (f.data.len() - src_off).min(CLUSTER);
            let dst = (DATA_START_SECTOR + (c - 2) * SEC_PER_CLUSTER) * SECTOR;
            disk[dst..dst + n].copy_from_slice(&f.data[src_off..src_off + n]);
        }
        // directory entry
        let name83 = to_83(f.name);
        let e = root_entry_mut(&mut disk, next_entry);
        e[0..11].copy_from_slice(&name83);
        e[11] = 0x21; // read-only + archive
        e[26..28].copy_from_slice(&(next_cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&(f.data.len() as u32).to_le_bytes());
        next_cluster += clusters;
        next_entry += 1;
    }

    disk
}

/// Write FAT12 entry `n` (12-bit packing: 1.5 bytes per entry).
fn fat_set(disk: &mut [u8], n: usize, val: u16) {
    let fat = SECTOR; // FAT starts at sector 1
    let off = fat + n + n / 2;
    if n % 2 == 0 {
        disk[off] = (val & 0xFF) as u8;
        disk[off + 1] = (disk[off + 1] & 0xF0) | ((val >> 8) & 0x0F) as u8;
    } else {
        disk[off] = (disk[off] & 0x0F) | (((val & 0x0F) as u8) << 4);
        disk[off + 1] = (val >> 4) as u8;
    }
}

fn root_entry_mut(disk: &mut [u8], i: usize) -> &mut [u8] {
    let root = (1 + FAT_SECTORS) * SECTOR;
    &mut disk[root + i * 32..root + (i + 1) * 32]
}

/// "name.ext" → padded upper-case 8.3 (11 bytes).
fn to_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None => (name, ""),
    };
    let clean = |c: u8| {
        let c = c.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' { c } else { b'_' }
    };
    for (i, b) in stem.bytes().take(8).enumerate() {
        out[i] = clean(b);
    }
    for (i, b) in ext.bytes().take(3).enumerate() {
        out[8 + i] = clean(b);
    }
    out
}
