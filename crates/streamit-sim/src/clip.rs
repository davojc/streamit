//! Length of a video file, read from its mp4/mov header.
use std::path::Path;
use std::time::Duration;

/// What an unreadable (or zero-length) file is taken to last.
pub const FALLBACK: Duration = Duration::from_secs(30);

/// The movie duration from the file's header, or 30 s on any failure. Reads only the
/// header (the media data is skipped), so it is cheap enough to call inline.
pub fn clip_length(path: &Path) -> Duration {
    read(path).filter(|d| !d.is_zero()).unwrap_or(FALLBACK)
}

fn read(path: &Path) -> Option<Duration> {
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).ok()?;
    Some(reader.duration())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    /// A fresh directory under the system temp dir, unique per test name and process.
    pub fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("streamit-sim-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    /// A minimal valid mp4: `ftyp` and a `moov` holding only a version-0 `mvhd` whose
    /// duration is `millis` (timescale 1000).
    pub fn mp4_bytes(millis: u32) -> Vec<u8> {
        let mut ftyp = b"isom".to_vec();
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        let mut mvhd = vec![0u8; 4]; // version 0, flags 0
        mvhd.extend_from_slice(&[0; 8]); // creation, modification
        mvhd.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd.extend_from_slice(&millis.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        mvhd.extend_from_slice(&[0; 10]); // reserved
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            mvhd.extend_from_slice(&v.to_be_bytes()); // identity matrix
        }
        mvhd.extend_from_slice(&[0; 24]); // pre-defined
        mvhd.extend_from_slice(&2u32.to_be_bytes()); // next track id
        let mut out = boxed(b"ftyp", &ftyp);
        out.extend(boxed(b"moov", &boxed(b"mvhd", &mvhd)));
        out
    }
}
