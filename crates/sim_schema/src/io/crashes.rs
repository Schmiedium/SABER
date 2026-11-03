//! The crash-dump stream `node_{rank}_a{attempt}_p{part}_crashes.zst` (subsystems §5.4, IB-10).
//!
//! Layout, all integers little-endian:
//!
//! ```text
//! [u32 len][FileHeader JSON]                          format "crashes"
//! ( [b'D'][u32 len][zstd(bincode CrashDump)] )*       one record per dump, appended as runs crash
//! [b'I'][u32 len][bincode Vec<CrashIndexEntry>]       written by finish()
//! [u64 offset of the b'I'][b"SABERIDX"]               16-byte trailer
//! ```
//!
//! A reader opens the trailer and index to seek straight to one dump (the studio's crash list, the
//! query service's `DoGet`). A stream whose writer never finished (a killed job's `.partial`) has
//! no trailer; [`CrashReader::open`] then scans the records from the start and stops at the first
//! incomplete one, so every whole dump stays readable.

use std::io::{Read, Seek, SeekFrom, Write};

use serde::{Deserialize, Serialize};

use super::TableError;
use crate::{
    CRASHES_FORMAT, CRASHES_SCHEMA_VERSION, CrashDump, CrashKind, CrashReason, FileHeader,
    SimStatus, TickFrame,
};

const RECORD: u8 = b'D';
const INDEX: u8 = b'I';
const TRAILER_MAGIC: &[u8; 8] = b"SABERIDX";
const TRAILER_LEN: u64 = 16;
/// zstd level of each dump (the library default).
const ZSTD_LEVEL: i32 = 3;

/// One dump in the index: enough to list crashes without decoding any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashIndexEntry {
    /// The run.
    pub run_id: u64,
    /// Its parameter row.
    pub param_set_id: u32,
    /// Its replicate seed.
    pub replicate_seed: u64,
    /// How it ended.
    pub status: SimStatus,
    /// Why.
    pub kind: CrashKind,
    /// The crashed tick.
    pub tick: u64,
    /// Byte offset of the record.
    pub offset: u64,
    /// Compressed length of the dump.
    pub len: u32,
}

/// `CrashDump` as encoded: bincode needs every field present, and the header's optional fields
/// are skipped when empty, so the header travels as JSON.
#[derive(Serialize)]
struct WireRef<'a> {
    header: String,
    run_id: u64,
    param_set_id: u32,
    replicate_seed: u64,
    params: &'a [(String, f64)],
    status: SimStatus,
    reason: &'a CrashReason,
    panic: &'a Option<String>,
    frames: &'a [TickFrame],
}

#[derive(Deserialize)]
struct Wire {
    header: String,
    run_id: u64,
    param_set_id: u32,
    replicate_seed: u64,
    params: Vec<(String, f64)>,
    status: SimStatus,
    reason: CrashReason,
    panic: Option<String>,
    frames: Vec<TickFrame>,
}

fn config() -> bincode::config::Configuration {
    bincode::config::standard()
}

fn corrupt(m: impl std::fmt::Display) -> TableError {
    TableError::Corrupt(m.to_string())
}

fn header_json(h: &FileHeader) -> String {
    serde_json::to_string(h).expect("a header serialises")
}

fn parse_header(json: &[u8]) -> Result<FileHeader, TableError> {
    serde_json::from_slice(json).map_err(|e| TableError::HeaderJson(e.to_string()))
}

/// Encode one dump as its compressed payload.
pub fn encode_dump(dump: &CrashDump) -> Result<Vec<u8>, TableError> {
    let wire = WireRef {
        header: header_json(&dump.header),
        run_id: dump.run_id,
        param_set_id: dump.param_set_id,
        replicate_seed: dump.replicate_seed,
        params: &dump.params,
        status: dump.status,
        reason: &dump.reason,
        panic: &dump.panic,
        frames: &dump.frames,
    };
    let raw = bincode::serde::encode_to_vec(&wire, config()).map_err(corrupt)?;
    Ok(zstd::bulk::compress(&raw, ZSTD_LEVEL)?)
}

/// Decode a compressed payload written by [`encode_dump`].
pub fn decode_dump(payload: &[u8]) -> Result<CrashDump, TableError> {
    let raw = zstd::stream::decode_all(payload)?;
    let (w, used): (Wire, usize) =
        bincode::serde::decode_from_slice(&raw, config()).map_err(corrupt)?;
    if used != raw.len() {
        return Err(corrupt("trailing bytes after a crash dump"));
    }
    Ok(CrashDump {
        header: parse_header(w.header.as_bytes())?,
        run_id: w.run_id,
        param_set_id: w.param_set_id,
        replicate_seed: w.replicate_seed,
        params: w.params,
        status: w.status,
        reason: w.reason,
        panic: w.panic,
        frames: w.frames,
    })
}

/// Appends dumps to a crash stream; [`CrashWriter::finish`] writes the index.
pub struct CrashWriter<W: Write> {
    out: W,
    pos: u64,
    index: Vec<CrashIndexEntry>,
}

impl<W: Write> CrashWriter<W> {
    /// Start a stream with this header (format `crashes`).
    pub fn new(mut out: W, header: &FileHeader) -> Result<Self, TableError> {
        header
            .check(CRASHES_FORMAT, CRASHES_SCHEMA_VERSION)
            .map_err(TableError::Header)?;
        let json = header_json(header);
        let len = u32::try_from(json.len()).map_err(corrupt)?;
        out.write_all(&len.to_le_bytes())?;
        out.write_all(json.as_bytes())?;
        Ok(Self {
            out,
            pos: 4 + json.len() as u64,
            index: Vec::new(),
        })
    }

    /// Append one dump. Returns its compressed size.
    pub fn append(&mut self, dump: &CrashDump) -> Result<u32, TableError> {
        let payload = encode_dump(dump)?;
        let len = u32::try_from(payload.len()).map_err(corrupt)?;
        self.out.write_all(&[RECORD])?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(&payload)?;
        self.index.push(CrashIndexEntry {
            run_id: dump.run_id,
            param_set_id: dump.param_set_id,
            replicate_seed: dump.replicate_seed,
            status: dump.status,
            kind: dump.reason.kind,
            tick: dump.reason.tick,
            offset: self.pos,
            len,
        });
        self.pos += 5 + u64::from(len);
        Ok(len)
    }

    /// Dumps appended so far.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether nothing was appended.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Write the index and trailer, flush, and hand back the output.
    pub fn finish(mut self) -> Result<W, TableError> {
        let index = bincode::serde::encode_to_vec(&self.index, config()).map_err(corrupt)?;
        let len = u32::try_from(index.len()).map_err(corrupt)?;
        self.out.write_all(&[INDEX])?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(&index)?;
        self.out.write_all(&self.pos.to_le_bytes())?;
        self.out.write_all(TRAILER_MAGIC)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Reads a crash stream: its header, its index, and single dumps by seeking.
pub struct CrashReader<R: Read + Seek> {
    input: R,
    header: FileHeader,
    index: Vec<CrashIndexEntry>,
    finished: bool,
}

impl<R: Read + Seek> CrashReader<R> {
    /// Open a stream: check the header, then read the index from the trailer or, for an
    /// unfinished stream, by scanning the records.
    pub fn open(mut input: R) -> Result<Self, TableError> {
        let size = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(0))?;
        let hlen = u64::from(read_u32(&mut input)?);
        if 4 + hlen > size {
            return Err(corrupt("header longer than the file"));
        }
        let mut json = vec![0; hlen as usize];
        input.read_exact(&mut json)?;
        let header = parse_header(&json)?;
        header
            .check(CRASHES_FORMAT, CRASHES_SCHEMA_VERSION)
            .map_err(TableError::Header)?;
        let first = 4 + hlen;
        let mut reader = Self {
            input,
            header,
            index: Vec::new(),
            finished: false,
        };
        if let Some(index) = reader.read_trailer(first, size)? {
            reader.index = index;
            reader.finished = true;
        } else {
            reader.index = reader.scan(first, size)?;
        }
        Ok(reader)
    }

    fn read_trailer(
        &mut self,
        first: u64,
        size: u64,
    ) -> Result<Option<Vec<CrashIndexEntry>>, TableError> {
        if size < first + 5 + TRAILER_LEN {
            return Ok(None);
        }
        self.input.seek(SeekFrom::Start(size - TRAILER_LEN))?;
        let mut trailer = [0u8; 16];
        self.input.read_exact(&mut trailer)?;
        if &trailer[8..] != TRAILER_MAGIC {
            return Ok(None);
        }
        let at = u64::from_le_bytes(trailer[..8].try_into().expect("8 bytes"));
        if at < first || at + 5 > size - TRAILER_LEN {
            return Err(corrupt("index offset outside the file"));
        }
        self.input.seek(SeekFrom::Start(at))?;
        let mut tag = [0u8; 1];
        self.input.read_exact(&mut tag)?;
        if tag[0] != INDEX {
            return Err(corrupt("trailer does not point at the index"));
        }
        let len = u64::from(read_u32(&mut self.input)?);
        if at + 5 + len != size - TRAILER_LEN {
            return Err(corrupt("index length does not match the trailer"));
        }
        let mut bytes = vec![0; len as usize];
        self.input.read_exact(&mut bytes)?;
        let (index, used): (Vec<CrashIndexEntry>, usize) =
            bincode::serde::decode_from_slice(&bytes, config()).map_err(corrupt)?;
        if used != bytes.len() {
            return Err(corrupt("trailing bytes after the index"));
        }
        if index
            .iter()
            .any(|e| e.offset < first || e.offset + 5 + u64::from(e.len) > at)
        {
            return Err(corrupt("index entry outside the records"));
        }
        Ok(Some(index))
    }

    /// Index an unfinished stream by decoding each whole record from the start.
    fn scan(&mut self, first: u64, size: u64) -> Result<Vec<CrashIndexEntry>, TableError> {
        let mut index = Vec::new();
        let mut at = first;
        self.input.seek(SeekFrom::Start(at))?;
        while at + 5 <= size {
            let mut tag = [0u8; 1];
            self.input.read_exact(&mut tag)?;
            if tag[0] != RECORD {
                break;
            }
            let len = read_u32(&mut self.input)?;
            if at + 5 + u64::from(len) > size {
                break;
            }
            let mut payload = vec![0; len as usize];
            self.input.read_exact(&mut payload)?;
            let d = decode_dump(&payload)?;
            index.push(CrashIndexEntry {
                run_id: d.run_id,
                param_set_id: d.param_set_id,
                replicate_seed: d.replicate_seed,
                status: d.status,
                kind: d.reason.kind,
                tick: d.reason.tick,
                offset: at,
                len,
            });
            at += 5 + u64::from(len);
        }
        Ok(index)
    }

    /// The stream's header.
    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    /// Every dump in the stream, in write order.
    pub fn index(&self) -> &[CrashIndexEntry] {
        &self.index
    }

    /// Whether the writer finished the stream (it has an index footer).
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Read one dump.
    pub fn read(&mut self, entry: &CrashIndexEntry) -> Result<CrashDump, TableError> {
        self.input.seek(SeekFrom::Start(entry.offset))?;
        let mut tag = [0u8; 1];
        self.input.read_exact(&mut tag)?;
        let len = read_u32(&mut self.input)?;
        if tag[0] != RECORD || len != entry.len {
            return Err(corrupt(format!("no crash dump at offset {}", entry.offset)));
        }
        let mut payload = vec![0; len as usize];
        self.input.read_exact(&mut payload)?;
        let dump = decode_dump(&payload)?;
        if dump.run_id != entry.run_id {
            return Err(corrupt(format!(
                "index says run {} at offset {}, the dump is run {}",
                entry.run_id, entry.offset, dump.run_id
            )));
        }
        Ok(dump)
    }

    /// Read the dump of a run, if the stream has one.
    pub fn read_run(&mut self, run_id: u64) -> Result<Option<CrashDump>, TableError> {
        match self.index.iter().find(|e| e.run_id == run_id).copied() {
            Some(e) => self.read(&e).map(Some),
            None => Ok(None),
        }
    }
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, TableError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
