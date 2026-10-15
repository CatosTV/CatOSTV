//! Finding a face inside a font collection (`.ttc`).
//!
//! Arch's `inter-font` ships every static weight in one `Inter.ttc`. egui
//! loads one face per `FontData` (`FontData::index`), so the GUI needs the
//! index of e.g. "Inter SemiBold". This reads the sfnt `name` table, no
//! font crate needed.

fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(o)?,
        *b.get(o + 1)?,
        *b.get(o + 2)?,
        *b.get(o + 3)?,
    ]))
}

/// Offsets of the faces in a file: a TTC lists several, a plain
/// TTF/OTF has one at offset 0.
pub fn face_offsets(data: &[u8]) -> Vec<usize> {
    if data.get(0..4) == Some(b"ttcf") {
        let n = be32(data, 8).unwrap_or(0).min(256) as usize;
        (0..n)
            .filter_map(|i| be32(data, 12 + i * 4).map(|o| o as usize))
            .collect()
    } else {
        vec![0]
    }
}

/// Decode a `name` table string (platform 3 = UTF-16BE, platform 1 = Latin).
fn decode(platform: u16, raw: &[u8]) -> String {
    if platform == 3 || platform == 0 {
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_be_bytes(c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        raw.iter().map(|&b| b as char).collect()
    }
}

/// `(typographic family, typographic subfamily)` of the face at `offset`:
/// name IDs 16/17 when present, else 1/2.
pub fn face_names(data: &[u8], offset: usize) -> Option<(String, String)> {
    let num_tables = be16(data, offset + 4)? as usize;
    let mut name_off = None;
    for i in 0..num_tables.min(64) {
        let rec = offset + 12 + i * 16;
        if data.get(rec..rec + 4)? == b"name" {
            name_off = Some(be32(data, rec + 8)? as usize);
            break;
        }
    }
    let t = name_off?;
    let count = be16(data, t + 2)? as usize;
    let strings = t + be16(data, t + 4)? as usize;
    let mut ids: [Option<String>; 18] = Default::default();
    for i in 0..count.min(512) {
        let r = t + 6 + i * 12;
        let platform = be16(data, r)?;
        let lang = be16(data, r + 4)?;
        let id = be16(data, r + 6)? as usize;
        let len = be16(data, r + 8)? as usize;
        let off = be16(data, r + 10)? as usize;
        // Prefer Windows English (0x409); accept anything as a fallback.
        if id >= ids.len() || (platform == 3 && lang != 0x409 && ids[id].is_some()) {
            continue;
        }
        let raw = data.get(strings + off..strings + off + len)?;
        if ids[id].is_none() || (platform == 3 && lang == 0x409) {
            ids[id] = Some(decode(platform, raw));
        }
    }
    let family = ids[16].clone().or_else(|| ids[1].clone())?;
    let style = ids[17].clone().or_else(|| ids[2].clone())?;
    Some((family, style))
}

/// Index of the face whose typographic family/style match (case-insensitive).
pub fn find_face(data: &[u8], family: &str, style: &str) -> Option<u32> {
    face_offsets(data)
        .into_iter()
        .enumerate()
        .find(|(_, off)| {
            face_names(data, *off).is_some_and(|(f, s)| {
                f.eq_ignore_ascii_case(family) && s.eq_ignore_ascii_case(style)
            })
        })
        .map(|(i, _)| i as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal sfnt with only a `name` table (Windows UTF-16BE).
    fn face(names: &[(u16, &str)]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut recs = Vec::new();
        for (id, s) in names {
            let raw: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
            recs.push((
                3u16,
                1u16,
                0x409u16,
                *id,
                raw.len() as u16,
                strings.len() as u16,
            ));
            strings.extend(raw);
        }
        let mut name = Vec::new();
        name.extend(0u16.to_be_bytes());
        name.extend((recs.len() as u16).to_be_bytes());
        name.extend(((6 + recs.len() * 12) as u16).to_be_bytes());
        for r in &recs {
            for v in [r.0, r.1, r.2, r.3, r.4, r.5] {
                name.extend(v.to_be_bytes());
            }
        }
        name.extend(strings);
        let mut f = Vec::new();
        f.extend(0x0001_0000u32.to_be_bytes());
        f.extend(1u16.to_be_bytes());
        f.extend([0u8; 6]);
        f.extend(b"name");
        f.extend(0u32.to_be_bytes());
        f.extend(28u32.to_be_bytes()); // table starts right after the record
        f.extend((name.len() as u32).to_be_bytes());
        f.extend(name);
        f
    }

    #[test]
    fn plain_font_and_collection() {
        let regular = face(&[(1, "Inter"), (2, "Regular")]);
        assert_eq!(
            face_names(&regular, 0),
            Some(("Inter".into(), "Regular".into()))
        );
        assert_eq!(find_face(&regular, "inter", "regular"), Some(0));

        // A TTC with two faces; the second uses typographic names.
        let semi = face(&[
            (1, "Inter SemiBold"),
            (2, "Regular"),
            (16, "Inter"),
            (17, "SemiBold"),
        ]);
        let mut ttc = Vec::new();
        ttc.extend(b"ttcf");
        ttc.extend(0x0001_0000u32.to_be_bytes());
        ttc.extend(2u32.to_be_bytes());
        let o1 = 20usize;
        let o2 = o1 + regular.len();
        ttc.extend((o1 as u32).to_be_bytes());
        ttc.extend((o2 as u32).to_be_bytes());
        // Table offsets inside each face are relative to the file start.
        let fix = |mut f: Vec<u8>, base: usize| {
            let off = be32(&f, 12 + 8).unwrap() as usize + base;
            f[20..24].copy_from_slice(&(off as u32).to_be_bytes());
            f
        };
        ttc.extend(fix(regular.clone(), o1));
        ttc.extend(fix(semi, o2));
        assert_eq!(face_offsets(&ttc), vec![o1, o2]);
        assert_eq!(find_face(&ttc, "Inter", "SemiBold"), Some(1));
        assert_eq!(find_face(&ttc, "Inter", "Regular"), Some(0));
        assert_eq!(find_face(&ttc, "Inter", "Black"), None);
        // Garbage never panics.
        assert_eq!(find_face(b"ttcf\0\0\0\0\xff\xff\xff\xff", "a", "b"), None);
        assert_eq!(find_face(&[], "a", "b"), None);
    }
}
