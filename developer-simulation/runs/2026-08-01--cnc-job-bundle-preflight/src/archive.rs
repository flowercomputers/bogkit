use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const END_SIGNATURE: u32 = 0x0605_4b50;
const UTF8_FLAG: u16 = 0x0800;
const MAX_ARCHIVE_ENTRIES: usize = 1_001;
const MAX_MEMBER_NAME_BYTES: usize = 4_096;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub compressed_size: u64,
    pub size: u64,
    pub method: u16,
    pub encrypted: bool,
    pub crc32: u32,
    pub data_offset: u64,
}

pub struct Archive {
    file: File,
    pub entries: Vec<Entry>,
}

impl Archive {
    #[allow(clippy::too_many_lines)]
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let search_length = length.min(65_557);
        if search_length < 22 {
            return Err(invalid("archive is truncated: no end record"));
        }
        file.seek(SeekFrom::End(
            -i64::try_from(search_length).map_err(|_| invalid("archive tail is too large"))?,
        ))?;
        let mut tail = vec![
            0_u8;
            usize::try_from(search_length)
                .map_err(|_| { invalid("archive tail does not fit in memory") })?
        ];
        file.read_exact(&mut tail)?;
        let end_index = tail
            .windows(4)
            .rposition(|window| le_u32(window) == END_SIGNATURE)
            .ok_or_else(|| invalid("archive is truncated: no end record"))?;
        if tail.len() - end_index < 22 {
            return Err(invalid("archive is truncated: partial end record"));
        }
        let end = &tail[end_index..];
        let disk = le_u16(&end[4..6]);
        let central_disk = le_u16(&end[6..8]);
        let disk_entries = le_u16(&end[8..10]);
        let entry_count = le_u16(&end[10..12]);
        let central_size = u64::from(le_u32(&end[12..16]));
        let central_offset = u64::from(le_u32(&end[16..20]));
        let comment_length = usize::from(le_u16(&end[20..22]));
        if disk != 0 || central_disk != 0 || disk_entries != entry_count {
            return Err(invalid("multi-disk ZIP archives are unsupported"));
        }
        if usize::from(entry_count) > MAX_ARCHIVE_ENTRIES {
            return Err(invalid("archive exceeds the 1,001-member limit"));
        }
        if end.len() < 22 + comment_length {
            return Err(invalid("archive is truncated: partial ZIP comment"));
        }
        let central_end = central_offset
            .checked_add(central_size)
            .ok_or_else(|| invalid("central directory offset overflow"))?;
        if central_end > length {
            return Err(invalid(
                "archive is truncated: central directory exceeds file",
            ));
        }

        file.seek(SeekFrom::Start(central_offset))?;
        let mut entries = Vec::with_capacity(usize::from(entry_count).min(1_001));
        for _ in 0..entry_count {
            let mut fixed = [0_u8; 46];
            file.read_exact(&mut fixed)
                .map_err(|_| invalid("archive is truncated: partial central entry"))?;
            if le_u32(&fixed[..4]) != CENTRAL_SIGNATURE {
                return Err(invalid("invalid central directory signature"));
            }
            let flags = le_u16(&fixed[8..10]);
            let method = le_u16(&fixed[10..12]);
            let crc32 = le_u32(&fixed[16..20]);
            let compressed_size = u64::from(le_u32(&fixed[20..24]));
            let size = u64::from(le_u32(&fixed[24..28]));
            let name_length = usize::from(le_u16(&fixed[28..30]));
            let extra_length = usize::from(le_u16(&fixed[30..32]));
            let comment_length = usize::from(le_u16(&fixed[32..34]));
            let local_offset = u64::from(le_u32(&fixed[42..46]));
            if compressed_size == u64::from(u32::MAX)
                || size == u64::from(u32::MAX)
                || local_offset == u64::from(u32::MAX)
            {
                return Err(invalid("ZIP64 archives are unsupported by this prototype"));
            }
            if name_length > MAX_MEMBER_NAME_BYTES {
                return Err(invalid("member name exceeds the 4,096-byte limit"));
            }
            let mut name = vec![0_u8; name_length];
            file.read_exact(&mut name)
                .map_err(|_| invalid("archive is truncated: partial member name"))?;
            if flags & UTF8_FLAG == 0 || std::str::from_utf8(&name).is_err() {
                return Err(invalid("member name is not explicitly valid UTF-8"));
            }
            file.seek(SeekFrom::Current(
                i64::try_from(extra_length + comment_length)
                    .map_err(|_| invalid("central metadata is too large"))?,
            ))?;

            let return_position = file.stream_position()?;
            file.seek(SeekFrom::Start(local_offset))?;
            let mut local = [0_u8; 30];
            file.read_exact(&mut local)
                .map_err(|_| invalid("archive is truncated: partial local entry"))?;
            if le_u32(&local[..4]) != LOCAL_SIGNATURE {
                return Err(invalid("invalid local entry signature"));
            }
            let local_name_length = u64::from(le_u16(&local[26..28]));
            let local_extra_length = u64::from(le_u16(&local[28..30]));
            let data_offset = local_offset
                .checked_add(30)
                .and_then(|value| value.checked_add(local_name_length))
                .and_then(|value| value.checked_add(local_extra_length))
                .ok_or_else(|| invalid("local entry offset overflow"))?;
            let data_end = data_offset
                .checked_add(compressed_size)
                .ok_or_else(|| invalid("member data offset overflow"))?;
            if data_end > central_offset {
                return Err(invalid(
                    "archive is truncated: member data overlaps central directory",
                ));
            }
            file.seek(SeekFrom::Start(return_position))?;

            entries.push(Entry {
                name: String::from_utf8(name).expect("validated UTF-8"),
                compressed_size,
                size,
                method,
                encrypted: flags & 1 != 0,
                crc32,
                data_offset,
            });
        }
        if file.stream_position()? != central_end {
            return Err(invalid("central directory size does not match its entries"));
        }
        Ok(Self { file, entries })
    }

    pub fn stream_entry<W: Write>(
        &mut self,
        entry: &Entry,
        mut destination: Option<&mut W>,
    ) -> io::Result<StreamResult> {
        if entry.encrypted {
            return Err(invalid("encrypted member is unsupported"));
        }
        if entry.method != 0 || entry.compressed_size != entry.size {
            return Err(invalid("only uncompressed ZIP members are supported"));
        }
        self.file.seek(SeekFrom::Start(entry.data_offset))?;
        let mut remaining = entry.compressed_size;
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        let mut sha = crate::sha256::Sha256::new();
        let mut crc = Crc32::new();
        let mut count = 0_u64;
        while remaining != 0 {
            let amount = usize::try_from(remaining.min(buffer.len() as u64))
                .expect("bounded by buffer length");
            let read = self.file.read(&mut buffer[..amount])?;
            if read == 0 {
                return Err(invalid("archive is truncated inside member data"));
            }
            sha.update(&buffer[..read]);
            crc.update(&buffer[..read]);
            if let Some(writer) = destination.as_deref_mut() {
                writer.write_all(&buffer[..read])?;
            }
            let read = u64::try_from(read).expect("buffer length fits u64");
            count += read;
            remaining -= read;
        }
        Ok(StreamResult {
            bytes: count,
            sha256: crate::sha256::hex(&sha.finish()),
            crc32: crc.finish(),
        })
    }
}

#[derive(Debug)]
pub struct StreamResult {
    pub bytes: u64,
    pub sha256: String,
    pub crc32: u32,
}

pub struct Writer {
    file: File,
    entries: Vec<WrittenEntry>,
}

struct WrittenEntry {
    name: String,
    size: u32,
    crc32: u32,
    local_offset: u32,
}

impl Writer {
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::create(path)?,
            entries: Vec::new(),
        })
    }

    pub fn member(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let mut crc = Crc32::new();
        crc.update(data);
        let size = u32::try_from(data.len()).map_err(|_| invalid("fixture member too large"))?;
        self.header(name, size, crc.finish())?;
        self.file.write_all(data)
    }

    pub fn sparse_zero_member(&mut self, name: &str, size: u32, crc32: u32) -> io::Result<()> {
        self.header(name, size, crc32)?;
        self.file.seek(SeekFrom::Current(i64::from(size)))?;
        Ok(())
    }

    fn header(&mut self, name: &str, size: u32, crc32: u32) -> io::Result<()> {
        let name_length = u16::try_from(name.len()).map_err(|_| invalid("member name too long"))?;
        let local_offset = u32::try_from(self.file.stream_position()?)
            .map_err(|_| invalid("fixture exceeds classic ZIP offsets"))?;
        write_u32(&mut self.file, LOCAL_SIGNATURE)?;
        write_u16(&mut self.file, 20)?;
        write_u16(&mut self.file, UTF8_FLAG)?;
        write_u16(&mut self.file, 0)?;
        write_u16(&mut self.file, 0)?;
        write_u16(&mut self.file, 0)?;
        write_u32(&mut self.file, crc32)?;
        write_u32(&mut self.file, size)?;
        write_u32(&mut self.file, size)?;
        write_u16(&mut self.file, name_length)?;
        write_u16(&mut self.file, 0)?;
        self.file.write_all(name.as_bytes())?;
        self.entries.push(WrittenEntry {
            name: name.to_owned(),
            size,
            crc32,
            local_offset,
        });
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let central_offset = u32::try_from(self.file.stream_position()?)
            .map_err(|_| invalid("fixture exceeds classic ZIP offsets"))?;
        for entry in &self.entries {
            write_u32(&mut self.file, CENTRAL_SIGNATURE)?;
            write_u16(&mut self.file, 20)?;
            write_u16(&mut self.file, 20)?;
            write_u16(&mut self.file, UTF8_FLAG)?;
            write_u16(&mut self.file, 0)?;
            write_u16(&mut self.file, 0)?;
            write_u16(&mut self.file, 0)?;
            write_u32(&mut self.file, entry.crc32)?;
            write_u32(&mut self.file, entry.size)?;
            write_u32(&mut self.file, entry.size)?;
            write_u16(
                &mut self.file,
                u16::try_from(entry.name.len()).map_err(|_| invalid("member name too long"))?,
            )?;
            write_u16(&mut self.file, 0)?;
            write_u16(&mut self.file, 0)?;
            write_u16(&mut self.file, 0)?;
            write_u16(&mut self.file, 0)?;
            write_u32(&mut self.file, 0)?;
            write_u32(&mut self.file, entry.local_offset)?;
            self.file.write_all(entry.name.as_bytes())?;
        }
        let central_end = u32::try_from(self.file.stream_position()?)
            .map_err(|_| invalid("fixture exceeds classic ZIP offsets"))?;
        let count = u16::try_from(self.entries.len()).map_err(|_| invalid("too many members"))?;
        write_u32(&mut self.file, END_SIGNATURE)?;
        write_u16(&mut self.file, 0)?;
        write_u16(&mut self.file, 0)?;
        write_u16(&mut self.file, count)?;
        write_u16(&mut self.file, count)?;
        write_u32(&mut self.file, central_end - central_offset)?;
        write_u32(&mut self.file, central_offset)?;
        write_u16(&mut self.file, 0)?;
        self.file.flush()
    }
}

pub struct Crc32(u32);

impl Crc32 {
    pub fn new() -> Self {
        Self(0xffff_ffff)
    }

    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.0 ^= u32::from(byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(self.0 & 1)));
            }
        }
    }

    pub fn finish(self) -> u32 {
        !self.0
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes[..2].try_into().expect("two bytes"))
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().expect("four bytes"))
}

fn write_u16(writer: &mut File, value: u16) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u32(writer: &mut File, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}
