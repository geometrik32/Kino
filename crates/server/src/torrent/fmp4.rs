//! Fragmented MP4 (`ftyp`/`moov` + `moof`/`mdat`) parser and stream splicer.
//!
//! Used by the pause-resume cache to trim a pre-transcoded prefix clip cleanly
//! at GOP (`moof`) boundaries, fingerprint its trailing cut-point IDR sample,
//! verify decoder configuration compatibility (`moov`), and rebase a live
//! continuation stream's `mfhd` sequence numbers and `tfdt` decode timestamps
//! without re-encoding or breaking progressive HTTP `<video>` playback.

use std::collections::HashMap;

use anyhow::{bail, ensure};
use sha2::{Digest, Sha256};

const MAX_BOX_BYTES: usize = 64 * 1024 * 1024;
const MIN_READY_DURATION_SECS: f64 = 6.0;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrackCodecInfo {
    pub track_id: u32,
    pub timescale: u32,
    pub stsd: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PreparedReadySegment {
    pub bytes: Vec<u8>,
    pub tracks: Vec<TrackCodecInfo>,
    pub last_seq: u32,
    pub track_ends: Vec<(u32, u64)>,
    pub duration_secs: f64,
    pub split_idr_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TrackFragInfo {
    pub track_id: u32,
    pub base_dts: u64,
    pub total_duration: u64,
}

#[derive(Debug, Clone)]
pub struct MoofInfo {
    pub seq: u32,
    pub tracks: Vec<TrackFragInfo>,
}

/// Incremental reader that buffers streamed bytes into complete top-level MP4 boxes.
#[derive(Default)]
pub struct BoxStreamReader {
    buf: Vec<u8>,
}

impl BoxStreamReader {
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    pub fn next_box(&mut self) -> anyhow::Result<Option<([u8; 4], Vec<u8>)>> {
        if self.buf.len() < 8 {
            return Ok(None);
        }
        let (total_len, typ) = parse_box_header(&self.buf)?;
        let Some(total_len) = total_len else {
            return Ok(None);
        };
        if self.buf.len() < total_len {
            return Ok(None);
        }
        let bx = self.buf[..total_len].to_vec();
        self.buf.drain(..total_len);
        Ok(Some((typ, bx)))
    }
}

fn parse_box_header(data: &[u8]) -> anyhow::Result<(Option<usize>, [u8; 4])> {
    if data.len() < 8 {
        return Ok((None, [0; 4]));
    }
    let sz32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let typ = [data[4], data[5], data[6], data[7]];
    let (hdr, total_u64) = if sz32 == 1 {
        if data.len() < 16 {
            return Ok((None, typ));
        }
        let sz64 = u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        (16usize, sz64)
    } else {
        (8usize, u64::from(sz32))
    };
    ensure!(total_u64 >= hdr as u64, "invalid MP4 box size {total_u64}");
    ensure!(
        total_u64 <= MAX_BOX_BYTES as u64,
        "MP4 box exceeds sanity limit: {total_u64}"
    );
    Ok((Some(total_u64 as usize), typ))
}

fn iter_children(mut body: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let (Some(total), typ) = parse_box_header(body).ok()? else {
            return None;
        };
        if total > body.len() {
            return None;
        }
        let bx = &body[..total];
        body = &body[total..];
        Some((typ, bx))
    })
}

fn box_payload(bx: &[u8]) -> &[u8] {
    let hdr = if bx.len() >= 16 && u32::from_be_bytes([bx[0], bx[1], bx[2], bx[3]]) == 1 {
        16
    } else {
        8
    };
    bx.get(hdr..).unwrap_or(&[])
}

fn read_u32(slice: &[u8], off: usize) -> Option<u32> {
    let b = slice.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_i32(slice: &[u8], off: usize) -> Option<i32> {
    let b = slice.get(off..off + 4)?;
    Some(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64(slice: &[u8], off: usize) -> Option<u64> {
    let b = slice.get(off..off + 8)?;
    Some(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// Extracts track IDs, timescales and sample descriptions (`stsd`) from a `moov` box.
pub fn extract_tracks(moov: &[u8]) -> Vec<TrackCodecInfo> {
    let mut out = Vec::new();
    for (typ, trak) in iter_children(box_payload(moov)) {
        if &typ != b"trak" {
            continue;
        }
        let mut track_id = None;
        let mut timescale = None;
        let mut stsd = None;
        for (ctyp, child) in iter_children(box_payload(trak)) {
            if &ctyp == b"tkhd" {
                let p = box_payload(child);
                if let Some(&ver) = p.first() {
                    let id_off = if ver == 1 { 20 } else { 12 };
                    track_id = read_u32(p, id_off);
                }
            } else if &ctyp == b"mdia" {
                for (mtyp, mchild) in iter_children(box_payload(child)) {
                    if &mtyp == b"mdhd" {
                        let p = box_payload(mchild);
                        if let Some(&ver) = p.first() {
                            let ts_off = if ver == 1 { 20 } else { 12 };
                            timescale = read_u32(p, ts_off);
                        }
                    } else if &mtyp == b"minf" {
                        for (ntyp, nchild) in iter_children(box_payload(mchild)) {
                            if &ntyp == b"stbl" {
                                for (styp, schild) in iter_children(box_payload(nchild)) {
                                    if &styp == b"stsd" {
                                        stsd = Some(schild.to_vec());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if let (Some(track_id), Some(timescale), Some(stsd)) = (track_id, timescale, stsd) {
            out.push(TrackCodecInfo {
                track_id,
                timescale,
                stsd,
            });
        }
    }
    out.sort_by_key(|t| t.track_id);
    out
}

struct ParsedTraf {
    track_id: u32,
    base_dts: u64,
    total_duration: u64,
    tfdt_abs_off: usize,
    tfdt_v1: bool,
    first_data_offset: Option<i32>,
    first_sample_size: Option<u32>,
}

fn parse_traf(moof: &[u8], traf_start: usize, traf: &[u8]) -> anyhow::Result<ParsedTraf> {
    let traf_hdr = if u32::from_be_bytes([traf[0], traf[1], traf[2], traf[3]]) == 1 {
        16
    } else {
        8
    };
    let mut track_id = None;
    let mut default_dur = 0u32;
    let mut default_sz = 0u32;
    let mut base_dts = None;
    let mut tfdt_abs_off = None;
    let mut tfdt_v1 = true;
    let mut total_duration = 0u64;
    let mut first_data_offset = None;
    let mut first_sample_size = None;

    let mut pos = traf_start + traf_hdr;
    let traf_end = traf_start + traf.len();
    while pos + 8 <= traf_end {
        let (Some(csz), ctyp) = parse_box_header(&moof[pos..traf_end])? else {
            break;
        };
        if pos + csz > traf_end {
            break;
        }
        let child = &moof[pos..pos + csz];
        let payload = box_payload(child);
        if &ctyp == b"tfhd" {
            let flags = read_u32(payload, 0).unwrap_or(0) & 0x00ff_ffff;
            track_id = read_u32(payload, 4);
            let mut p = 8;
            if flags & 0x01 != 0 {
                p += 8;
            }
            if flags & 0x02 != 0 {
                p += 4;
            }
            if flags & 0x08 != 0 {
                default_dur = read_u32(payload, p).unwrap_or(0);
                p += 4;
            }
            if flags & 0x10 != 0 {
                default_sz = read_u32(payload, p).unwrap_or(0);
            }
        } else if &ctyp == b"tfdt" {
            let ver = payload.first().copied().unwrap_or(0);
            let payload_abs_off = pos + (child.len() - payload.len());
            if ver == 1 {
                base_dts = read_u64(payload, 4);
                tfdt_abs_off = Some(payload_abs_off + 4);
                tfdt_v1 = true;
            } else {
                base_dts = read_u32(payload, 4).map(u64::from);
                tfdt_abs_off = Some(payload_abs_off + 4);
                tfdt_v1 = false;
            }
        } else if &ctyp == b"trun" {
            let flags = read_u32(payload, 0).unwrap_or(0) & 0x00ff_ffff;
            let count = read_u32(payload, 4).unwrap_or(0);
            let mut p = 8;
            if flags & 0x01 != 0 {
                if first_data_offset.is_none() {
                    first_data_offset = read_i32(payload, p);
                }
                p += 4;
            }
            if flags & 0x04 != 0 {
                p += 4;
            }
            for idx in 0..count {
                if p >= payload.len() {
                    break;
                }
                let dur = if flags & 0x100 != 0 {
                    let d = read_u32(payload, p).unwrap_or(default_dur);
                    p += 4;
                    d
                } else {
                    default_dur
                };
                let sz = if flags & 0x200 != 0 {
                    let s = read_u32(payload, p).unwrap_or(default_sz);
                    p += 4;
                    s
                } else {
                    default_sz
                };
                if idx == 0 && first_sample_size.is_none() {
                    first_sample_size = Some(sz);
                }
                if flags & 0x400 != 0 {
                    p += 4;
                }
                if flags & 0x800 != 0 {
                    p += 4;
                }
                total_duration = total_duration.saturating_add(u64::from(dur));
            }
        }
        pos += csz;
    }

    let Some(track_id) = track_id else {
        bail!("traf missing tfhd track_id");
    };
    let Some(base_dts) = base_dts else {
        bail!("traf missing tfdt");
    };
    let Some(tfdt_abs_off) = tfdt_abs_off else {
        bail!("traf missing tfdt offset");
    };
    Ok(ParsedTraf {
        track_id,
        base_dts,
        total_duration,
        tfdt_abs_off,
        tfdt_v1,
        first_data_offset,
        first_sample_size,
    })
}

fn parse_moof_internal(moof: &[u8]) -> anyhow::Result<(u32, usize, Vec<ParsedTraf>)> {
    ensure!(moof.len() >= 8, "truncated moof");
    let hdr = if u32::from_be_bytes([moof[0], moof[1], moof[2], moof[3]]) == 1 {
        16
    } else {
        8
    };
    let mut seq = None;
    let mut seq_abs_off = None;
    let mut trafs = Vec::new();
    let mut pos = hdr;
    while pos + 8 <= moof.len() {
        let (Some(sz), typ) = parse_box_header(&moof[pos..])? else {
            break;
        };
        if pos + sz > moof.len() {
            break;
        }
        let child = &moof[pos..pos + sz];
        if &typ == b"mfhd" {
            let p = box_payload(child);
            seq = read_u32(p, 4);
            seq_abs_off = Some(pos + (child.len() - p.len()) + 4);
        } else if &typ == b"traf" {
            trafs.push(parse_traf(moof, pos, child)?);
        }
        pos += sz;
    }
    let Some(seq) = seq else {
        bail!("moof missing mfhd sequence_number");
    };
    let Some(seq_abs_off) = seq_abs_off else {
        bail!("moof missing mfhd offset");
    };
    Ok((seq, seq_abs_off, trafs))
}

pub fn parse_moof(moof: &[u8]) -> anyhow::Result<MoofInfo> {
    let (seq, _, trafs) = parse_moof_internal(moof)?;
    Ok(MoofInfo {
        seq,
        tracks: trafs
            .into_iter()
            .map(|t| TrackFragInfo {
                track_id: t.track_id,
                base_dts: t.base_dts,
                total_duration: t.total_duration,
            })
            .collect(),
    })
}

/// Computes the hex SHA-256 of the first sample of `track_id` inside `(moof, mdat)`.
pub fn first_sample_sha256(moof: &[u8], mdat: &[u8], track_id: u32) -> Option<String> {
    let (_, _, trafs) = parse_moof_internal(moof).ok()?;
    let traf = trafs.into_iter().find(|t| t.track_id == track_id)?;
    let data_offset = traf.first_data_offset?;
    let sample_sz = traf.first_sample_size? as usize;
    if sample_sz == 0 {
        return None;
    }
    let rel = isize::try_from(data_offset).ok()? - isize::try_from(moof.len()).ok()?;
    if rel < 8 {
        return None;
    }
    let start = usize::try_from(rel).ok()?;
    let end = start.checked_add(sample_sz)?;
    let sample = mdat.get(start..end)?;
    Some(hex::encode(Sha256::digest(sample)))
}

/// Rewrites `mfhd.sequence_number` and every track's `tfdt` by `dts_deltas` in place.
pub fn patch_moof(moof: &mut [u8], new_seq: u32, dts_deltas: &HashMap<u32, i128>) -> anyhow::Result<()> {
    let (_, seq_off, trafs) = parse_moof_internal(moof)?;
    moof[seq_off..seq_off + 4].copy_from_slice(&new_seq.to_be_bytes());
    for t in trafs {
        let delta = dts_deltas.get(&t.track_id).copied().unwrap_or(0);
        let shifted = i128::from(t.base_dts) + delta;
        ensure!(shifted >= 0, "negative shifted tfdt for track {}", t.track_id);
        let new_dts = u64::try_from(shifted)?;
        if t.tfdt_v1 {
            moof[t.tfdt_abs_off..t.tfdt_abs_off + 8].copy_from_slice(&new_dts.to_be_bytes());
        } else {
            let d32 = u32::try_from(new_dts)?;
            moof[t.tfdt_abs_off..t.tfdt_abs_off + 4].copy_from_slice(&d32.to_be_bytes());
        }
    }
    Ok(())
}

/// Trims a generated fragmented MP4 to complete whole GOP fragments (`moof`+`mdat`),
/// returning the clean `ready.mp4` bytes and metadata needed to splice a live continuation.
pub fn prepare_ready_segment(raw_fmp4: &[u8]) -> anyhow::Result<PreparedReadySegment> {
    let mut reader = BoxStreamReader::default();
    reader.push(raw_fmp4);
    let mut ftyp = None;
    let mut moov = None;
    let mut frags: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut pending_moof = None;

    while let Some((typ, bx)) = reader.next_box()? {
        match &typ {
            b"ftyp" if ftyp.is_none() => ftyp = Some(bx),
            b"moov" if moov.is_none() => moov = Some(bx),
            b"moof" => pending_moof = Some(bx),
            b"mdat" => {
                if let Some(m) = pending_moof.take() {
                    frags.push((m, bx));
                }
            }
            _ => {}
        }
    }

    let Some(ftyp) = ftyp else {
        bail!("missing ftyp in transcoded prefix");
    };
    let Some(moov) = moov else {
        bail!("missing moov in transcoded prefix");
    };
    let tracks = extract_tracks(&moov);
    let Some(video_ts) = tracks.iter().find(|t| t.track_id == 1).map(|t| t.timescale) else {
        bail!("missing video track 1 in moov");
    };
    ensure!(video_ts > 0, "invalid video timescale");
    ensure!(
        frags.len() >= 2,
        "need at least 2 fragments to drop trailing partial GOP"
    );

    // Walk backward until the dropped boundary fragment contains a video track keyframe sample.
    let mut split_idx = frags.len() - 1;
    let mut split_idr_hash = None;
    while split_idx >= 1 {
        if let Some(h) = first_sample_sha256(&frags[split_idx].0, &frags[split_idx].1, 1) {
            split_idr_hash = Some(h);
            break;
        }
        split_idx -= 1;
    }
    ensure!(split_idx >= 1, "no clean video keyframe split fragment found");

    let kept = &frags[..split_idx];
    let mut out =
        Vec::with_capacity(ftyp.len() + moov.len() + kept.iter().map(|(a, b)| a.len() + b.len()).sum::<usize>());
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);

    let mut last_seq = 0u32;
    let mut track_ends: HashMap<u32, u64> = HashMap::new();
    let mut video_ticks = 0u64;
    for (moof, mdat) in kept {
        let info = parse_moof(moof)?;
        last_seq = info.seq;
        for t in info.tracks {
            let end = t.base_dts.saturating_add(t.total_duration);
            track_ends.insert(t.track_id, end);
            if t.track_id == 1 {
                video_ticks = video_ticks.saturating_add(t.total_duration);
            }
        }
        out.extend_from_slice(moof);
        out.extend_from_slice(mdat);
    }

    let duration_secs = video_ticks as f64 / f64::from(video_ts);
    ensure!(
        duration_secs >= MIN_READY_DURATION_SECS,
        "ready segment too short ({duration_secs:.2}s)"
    );

    let mut ends_vec: Vec<(u32, u64)> = track_ends.into_iter().collect();
    ends_vec.sort_by_key(|(id, _)| *id);

    Ok(PreparedReadySegment {
        bytes: out,
        tracks,
        last_seq,
        track_ends: ends_vec,
        duration_secs,
        split_idr_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let total = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(total as usize);
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(typ);
        out.extend_from_slice(payload);
        out
    }

    fn make_full_box(typ: &[u8; 4], ver: u8, flags: u32, rest: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(4 + rest.len());
        p.push(ver);
        p.extend_from_slice(&flags.to_be_bytes()[1..4]);
        p.extend_from_slice(rest);
        make_box(typ, &p)
    }

    fn make_synthetic_moov() -> Vec<u8> {
        let mut tkhd1 = vec![0u8; 20];
        tkhd1[8..12].copy_from_slice(&1u32.to_be_bytes());
        let mut mdhd1 = vec![0u8; 16];
        mdhd1[8..12].copy_from_slice(&16000u32.to_be_bytes());
        let stsd1 = make_full_box(b"stsd", 0, 0, b"\x00\x00\x00\x01avc1_cfg");
        let stbl1 = make_box(b"stbl", &stsd1);
        let minf1 = make_box(b"minf", &stbl1);
        let mut mdia1 = make_full_box(b"mdhd", 0, 0, &mdhd1);
        mdia1.extend_from_slice(&minf1);
        let mut trak1_body = make_full_box(b"tkhd", 0, 1, &tkhd1);
        trak1_body.extend_from_slice(&make_box(b"mdia", &mdia1));
        make_box(b"moov", &make_box(b"trak", &trak1_body))
    }

    fn make_frag(seq: u32, vid_dts: u64, vid_dur: u32, vid_sample: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mfhd = make_full_box(b"mfhd", 0, 0, &seq.to_be_bytes());
        // Track 1 traf with placeholder data_offset (updated after measuring moof).
        let build_moof = |data_off: i32| {
            let mut tfhd_rest = Vec::new();
            tfhd_rest.extend_from_slice(&1u32.to_be_bytes());
            let tfhd = make_full_box(b"tfhd", 0, 0x02_0000, &tfhd_rest);
            let tfdt = make_full_box(b"tfdt", 1, 0, &vid_dts.to_be_bytes());
            let mut trun_rest = Vec::new();
            trun_rest.extend_from_slice(&1u32.to_be_bytes()); // sample_count = 1
            trun_rest.extend_from_slice(&data_off.to_be_bytes());
            trun_rest.extend_from_slice(&vid_dur.to_be_bytes());
            trun_rest.extend_from_slice(&(vid_sample.len() as u32).to_be_bytes());
            let trun = make_full_box(b"trun", 0, 0x0301, &trun_rest);
            let mut traf_body = tfhd;
            traf_body.extend_from_slice(&tfdt);
            traf_body.extend_from_slice(&trun);
            let mut moof_body = mfhd.clone();
            moof_body.extend_from_slice(&make_box(b"traf", &traf_body));
            make_box(b"moof", &moof_body)
        };
        let dummy = build_moof(0);
        let moof = build_moof((dummy.len() + 8) as i32);
        let mdat = make_box(b"mdat", vid_sample);
        (moof, mdat)
    }

    #[test]
    fn extracts_tracks_and_patches_spliced_moof() {
        let moov = make_synthetic_moov();
        let tracks = extract_tracks(&moov);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_id, 1);
        assert_eq!(tracks[0].timescale, 16000);

        let ftyp = make_box(b"ftyp", b"isom\x00\x00\x02\x00");
        let (m1, d1) = make_frag(1, 0, 64000, b"idr_frame_0");
        let (m2, d2) = make_frag(2, 64000, 64000, b"idr_frame_1");
        let (m3, d3) = make_frag(3, 128000, 8000, b"idr_frame_split");

        let mut raw = Vec::new();
        raw.extend_from_slice(&ftyp);
        raw.extend_from_slice(&moov);
        for (m, d) in [&(m1, d1), &(m2, d2), &(m3, d3)] {
            raw.extend_from_slice(m);
            raw.extend_from_slice(d);
        }

        let prep = prepare_ready_segment(&raw).unwrap();
        assert_eq!(prep.last_seq, 2);
        assert_eq!(prep.track_ends, vec![(1, 128000)]);
        assert!((prep.duration_secs - 8.0).abs() < 0.001);
        assert_eq!(
            prep.split_idr_hash.as_deref(),
            Some(hex::encode(Sha256::digest(b"idr_frame_split")).as_str())
        );

        let (mut m_cont, d_cont) = make_frag(2, 64000, 64000, b"idr_frame_split");
        assert_eq!(first_sample_sha256(&m_cont, &d_cont, 1), prep.split_idr_hash);
        let deltas = HashMap::from([(1u32, 128000i128 - 64000i128)]);
        patch_moof(&mut m_cont, 3, &deltas).unwrap();
        let parsed = parse_moof(&m_cont).unwrap();
        assert_eq!(parsed.seq, 3);
        assert_eq!(parsed.tracks[0].base_dts, 128000);
    }
}
