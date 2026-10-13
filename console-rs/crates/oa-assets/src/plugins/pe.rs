//! PE resource directory reader: finds `TYPE/NAME/LANG` leaves in a DLL.

/// One resource leaf: its path and where its bytes are in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub ty: String,
    pub name: String,
    pub lang: u32,
    pub offset: usize,
    pub size: usize,
}

struct Sections(Vec<(u32, u32, u32)>); // (virtual address, size, file offset)

impl Sections {
    fn file_offset(&self, rva: u32) -> Option<usize> {
        self.0
            .iter()
            .find(|&&(va, sz, _)| rva >= va && rva < va.wrapping_add(sz))
            .map(|&(va, _, raw)| (rva - va + raw) as usize)
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Every resource leaf of a PE file (32- or 64-bit), in directory order.
pub fn resources(b: &[u8]) -> Option<Vec<Leaf>> {
    if b.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(b, 0x3c)? as usize;
    if b.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let nsec = u16_at(b, coff + 2)? as usize;
    let opt_size = u16_at(b, coff + 16)? as usize;
    let opt = coff + 20;
    let dirs = match u16_at(b, opt)? {
        0x20b => opt + 112,
        0x10b => opt + 96,
        _ => return None,
    };
    let res_rva = u32_at(b, dirs + 2 * 8)?;
    let mut secs = Vec::with_capacity(nsec);
    for i in 0..nsec {
        let s = opt + opt_size + i * 40;
        let vsize = u32_at(b, s + 8)?;
        let va = u32_at(b, s + 12)?;
        let raw_size = u32_at(b, s + 16)?;
        let raw = u32_at(b, s + 20)?;
        secs.push((va, vsize.max(raw_size), raw));
    }
    let secs = Sections(secs);
    if res_rva == 0 {
        return Some(Vec::new());
    }
    let root = secs.file_offset(res_rva)?;
    let mut out = Vec::new();
    let name_of = |id: u32| -> Option<String> {
        if id & 0x8000_0000 == 0 {
            return Some(id.to_string());
        }
        let at = root + (id & 0x7fff_ffff) as usize;
        let n = u16_at(b, at)? as usize;
        let units: Vec<u16> = (0..n).map(|k| u16_at(b, at + 2 + 2 * k)).collect::<Option<_>>()?;
        Some(String::from_utf16_lossy(&units))
    };
    let entries = |dir: usize| -> Option<Vec<(u32, u32)>> {
        let n = u16_at(b, dir + 12)? as usize + u16_at(b, dir + 14)? as usize;
        (0..n.min(65536)).map(|k| Some((u32_at(b, dir + 16 + 8 * k)?, u32_at(b, dir + 20 + 8 * k)?))).collect()
    };
    for (tid, toff) in entries(root)? {
        if toff & 0x8000_0000 == 0 {
            continue;
        }
        let ty = name_of(tid)?;
        for (nid, noff) in entries(root + (toff & 0x7fff_ffff) as usize)? {
            if noff & 0x8000_0000 == 0 {
                continue;
            }
            let name = name_of(nid)?;
            for (lid, loff) in entries(root + (noff & 0x7fff_ffff) as usize)? {
                if loff & 0x8000_0000 != 0 {
                    continue;
                }
                let de = root + loff as usize;
                let (rva, size) = (u32_at(b, de)?, u32_at(b, de + 4)? as usize);
                let Some(offset) = secs.file_offset(rva) else { continue };
                out.push(Leaf { ty: ty.clone(), name: name.clone(), lang: lid & 0x7fff_ffff, offset, size });
            }
        }
    }
    Some(out)
}

/// The bytes of resource `ty/name/lang` (`lang` None: the first language).
/// Names compare exactly, then ignoring ASCII case.
pub fn find<'a>(b: &'a [u8], leaves: &[Leaf], ty: &str, name: &str, lang: Option<u32>) -> Option<&'a [u8]> {
    let hit = |exact: bool| {
        leaves.iter().find(|l| {
            let eq = |a: &str, z: &str| if exact { a == z } else { a.eq_ignore_ascii_case(z) };
            eq(&l.ty, ty) && eq(&l.name, name) && lang.is_none_or(|g| g == l.lang)
        })
    };
    let l = hit(true).or_else(|| hit(false))?;
    b.get(l.offset..l.offset + l.size)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal PE32+ with one section holding a resource tree:
    /// RCDATA(10)/42/1033 and "PNG"/"A.PNG"/1033.
    pub(crate) fn tiny_pe() -> Vec<u8> {
        tiny_pe_with(b"abcd", b"xyz")
    }

    /// [`tiny_pe`] with the given bytes as "PNG"/"A.PNG" and RCDATA/42.
    pub(crate) fn tiny_pe_with(png: &[u8], rc: &[u8]) -> Vec<u8> {
        let (a_at, r_at) = (0x400usize, 0x400 + png.len().div_ceil(16) * 16);
        let total = r_at + rc.len().div_ceil(16) * 16 + 16;
        let mut b = vec![0u8; total];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        let coff = 0x84;
        b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        b[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
        let opt = coff + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        let dirs = opt + 112;
        b[dirs + 16..dirs + 20].copy_from_slice(&0x1000u32.to_le_bytes());
        b[dirs + 20..dirs + 24].copy_from_slice(&0x200u32.to_le_bytes());
        let sec = opt + 240;
        b[sec..sec + 5].copy_from_slice(b".rsrc");
        let raw = (total - 0x200) as u32;
        b[sec + 8..sec + 12].copy_from_slice(&raw.to_le_bytes());
        b[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        b[sec + 16..sec + 20].copy_from_slice(&raw.to_le_bytes());
        b[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
        let r = 0x200;
        let w32 = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let w16 = |b: &mut Vec<u8>, o: usize, v: u16| b[o..o + 2].copy_from_slice(&v.to_le_bytes());
        // root: 1 named ("PNG"), 1 id (10)
        w16(&mut b, r + 12, 1);
        w16(&mut b, r + 14, 1);
        w32(&mut b, r + 16, 0x8000_0000 | 0x180);
        w32(&mut b, r + 20, 0x8000_0000 | 0x30);
        w32(&mut b, r + 24, 10);
        w32(&mut b, r + 28, 0x8000_0000 | 0x60);
        // "PNG" type dir at 0x30: one named entry "A.PNG" -> lang dir 0x48
        w16(&mut b, r + 0x30 + 12, 1);
        w32(&mut b, r + 0x30 + 16, 0x8000_0000 | 0x190);
        w32(&mut b, r + 0x30 + 20, 0x8000_0000 | 0x48);
        w16(&mut b, r + 0x48 + 14, 1);
        w32(&mut b, r + 0x48 + 16, 1033);
        w32(&mut b, r + 0x48 + 20, 0x90);
        // RCDATA type dir at 0x60: id 42 -> lang dir 0x78
        w16(&mut b, r + 0x60 + 14, 1);
        w32(&mut b, r + 0x60 + 16, 42);
        w32(&mut b, r + 0x60 + 20, 0x8000_0000 | 0x78);
        w16(&mut b, r + 0x78 + 14, 1);
        w32(&mut b, r + 0x78 + 16, 1033);
        w32(&mut b, r + 0x78 + 20, 0xa0);
        // data entries
        w32(&mut b, r + 0x90, 0x1000 + (a_at - r) as u32);
        w32(&mut b, r + 0x94, png.len() as u32);
        w32(&mut b, r + 0xa0, 0x1000 + (r_at - r) as u32);
        w32(&mut b, r + 0xa4, rc.len() as u32);
        b[a_at..a_at + png.len()].copy_from_slice(png);
        b[r_at..r_at + rc.len()].copy_from_slice(rc);
        // names
        for (at, s) in [(0x180, "PNG"), (0x190, "A.PNG")] {
            w16(&mut b, r + at, s.len() as u16);
            for (k, u) in s.encode_utf16().enumerate() {
                w16(&mut b, r + at + 2 + 2 * k, u);
            }
        }
        b
    }

    #[test]
    fn walks_named_and_numbered_resources() {
        let b = tiny_pe();
        let leaves = resources(&b).unwrap();
        assert_eq!(leaves.len(), 2);
        assert_eq!(find(&b, &leaves, "PNG", "A.PNG", Some(1033)), Some(&b"abcd"[..]));
        assert_eq!(find(&b, &leaves, "png", "a.png", None), Some(&b"abcd"[..]));
        assert_eq!(find(&b, &leaves, "10", "42", Some(1033)), Some(&b"xyz"[..]));
        assert_eq!(find(&b, &leaves, "10", "43", None), None);
        assert!(resources(b"not a pe").is_none());
    }
}
