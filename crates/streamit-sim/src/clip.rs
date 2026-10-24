//! Length of a video file, read from its mp4/mov header.
use std::path::Path;
use std::time::Duration;

/// What an unreadable (or zero-length) file is taken to last.
pub const FALLBACK: Duration = Duration::from_secs(30);

/// The movie duration from the file's header, or 30 s on any failure (the pretend
/// player's length). Reads only the header (the media data is skipped), so it is cheap
/// enough to call inline.
pub fn clip_length(path: &Path) -> Duration {
    read_length(path).unwrap_or(FALLBACK)
}

/// The movie duration from the file's header; `None` when the file is missing, is not a
/// readable mp4/mov, or says it lasts no time at all. For showing a real length.
pub fn read_length(path: &Path) -> Option<Duration> {
    read(path).filter(|d| !d.is_zero())
}

/// Never panics: the `mp4` crate's own `duration()` divides by the timescale (zero in a
/// corrupt header), so this does the arithmetic itself, and a panic while parsing a
/// hostile file is caught too.
fn read(path: &Path) -> Option<Duration> {
    std::panic::catch_unwind(|| {
        let file = std::fs::File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        let reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).ok()?;
        let (ticks, scale) = (reader.moov.mvhd.duration, u64::from(reader.moov.mvhd.timescale));
        if scale == 0 {
            return None;
        }
        // remainder < scale < 2^32, so the nanosecond product fits in a u64.
        let nanos = (ticks % scale).checked_mul(1_000_000_000)? / scale;
        Some(Duration::new(ticks / scale, nanos as u32))
    })
    .ok()
    .flatten()
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
        mp4_bytes_with_timescale(millis, 1000)
    }

    /// As `mp4_bytes`, with an explicit (possibly nonsensical) timescale.
    pub fn mp4_bytes_with_timescale(duration: u32, timescale: u32) -> Vec<u8> {
        let mut ftyp = b"isom".to_vec();
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        let mut mvhd = vec![0u8; 4]; // version 0, flags 0
        mvhd.extend_from_slice(&[0; 8]); // creation, modification
        mvhd.extend_from_slice(&timescale.to_be_bytes());
        mvhd.extend_from_slice(&duration.to_be_bytes());
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
