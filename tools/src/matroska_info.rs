use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::time::Duration;

use circular::Buffer;
use err_derive::Error;
use nom::{Err, Offset};

use matroska::ebml::ebml_header;
use matroska::elements::{segment, segment_element, SegmentElement, Uuid};
use matroska::serializer::ebml::EbmlSize;

#[derive(Debug, Error)]
pub enum InfoError {
    #[error(display = "expected file path")]
    NoPathReceived,
    #[error(display = "no more data to read or parse")]
    NoMoreData,
    #[error(display = "unable to parse header")]
    ParseHeader,
    #[error(display = "already got a SeekHead element")]
    SeekHeadElement,
    #[error(display = "already got an Info element")]
    InfoElement,
    #[error(display = "already got a Tracks element")]
    TracksElement,
    #[error(display = "unexpected element: {}", _0)]
    UnexpectedElement(String),
    #[error(display = "offset {:X?}: got unknown element: {:X?} {:#?}", _0, _1, _2)]
    UnknownElement(usize, u32, Option<usize>),
    #[error(display = "failed parsing: {}", _0)]
    Parse(String),
    #[error(display = "could not read the file: {}", _0)]
    Io(#[error(cause)] io::Error),
}

fn main() -> Result<(), InfoError> {
    pretty_env_logger::init();
    let mut args = env::args();
    let _ = args.next().expect("first arg is program path");
    let filename = args.next().ok_or(InfoError::NoPathReceived)?;

    run(&filename)
}

fn run(filename: &str) -> Result<(), InfoError> {
    let mut file = File::open(filename)?;

    let capacity = 5_242_880;
    let mut b = Buffer::with_capacity(capacity);

    // we write into the `&mut[u8]` returned by `space()`
    let sz = file.read(b.space())?;
    b.fill(sz);

    let length = {
        let res = ebml_header(b.data());
        if let Ok((remaining, header)) = res {
            println!("+ EBML head");
            println!("|+ EBML version: {}", header.version);
            println!("|+ EBML read version: {}", header.read_version);
            println!("|+ Maximum EBML ID length: {}", header.max_id_length);
            println!("|+ Maximum EBML size length: {}", header.max_size_length);
            println!("|+ Document type: {}", header.doc_type);
            println!("|+ Document type version: {}", header.doc_type_version);
            println!(
                "|+ Document type read version: {}",
                header.doc_type_read_version
            );

            b.data().offset(remaining)
        } else {
            return Err(InfoError::ParseHeader);
        }
    };

    let mut _consumed = length;
    b.consume(length);

    let length = {
        let res = segment(b.data());
        if let Ok((remaining, segment)) = res {
            println!("+ Segment, size {}", segment.1.unwrap_or(0));

            b.data().offset(remaining)
        } else {
            return Err(InfoError::ParseHeader);
        }
    };

    b.consume(length);

    // handle first elements
    let mut seek_head = None;
    let mut info = None;
    let mut tracks = None;

    loop {
        if seek_head.is_some() && info.is_some() && tracks.is_some() {
            break;
        }

        if b.available_space() == 0 {
            b.shift();
            if b.available_space() == 0 {
                println!("buffer is already full, cannot refill");
                break;
            }
        }

        // refill the buffer
        let sz = file.read(b.space())?;
        b.fill(sz);

        // if there's no more available data in the buffer after a write, that means we reached
        // the end of the file
        if b.available_data() == 0 {
            return Err(InfoError::NoMoreData);
        }

        let offset = {
            let (i, element) = match segment_element(b.data()) {
                Ok((i, o)) => (i, o),
                Err(Err::Error(e)) | Err(Err::Failure(e)) => {
                    return Err(InfoError::Parse(format!("{:?}", e)))
                }
                Err(Err::Incomplete(_i)) => continue,
            };

            match element {
                SegmentElement::SeekHead(s) => {
                    println!("|+ Seek head at {:#0x} size {}", 0x0, b.data().offset(i));
                    for seek in s.positions.iter() {
                        let element_size = seek.size(0x4DBB);
                        let id_size = seek.id.size(0x53AB);
                        let position_size = seek.position.size(0x53AC);

                        println!("| + Seek entry size {}", element_size);

                        // FIXME: Make the formatting similar to mkvinfo again
                        print!(
                            "|  + Seek ID: {} {} {} {}",
                            seek.id[0], seek.id[1], seek.id[2], seek.id[3]
                        );

                        let name = match seek.id {
                            [0x11, 0x4D, 0x9B, 0x74] => " (KaxSeekHead)",
                            [0x12, 0x54, 0xC3, 0x67] => " (KaxTags)",
                            [0x15, 0x49, 0xA9, 0x66] => " (KaxInfo)",
                            [0x16, 0x54, 0xAE, 0x6B] => " (KaxTracks)",
                            [0x1C, 0x53, 0xBB, 0x6B] => " (KaxCues)",
                            [0x1F, 0x43, 0xB6, 0x75] => " (KaxCluster)",
                            _ => "",
                        };

                        println!("{} at {:#0x} size {}", name, 0x0, id_size);
                        println!(
                            "|  + Seek position: {} size {}",
                            seek.position, position_size
                        );
                    }

                    if seek_head.is_some() {
                        return Err(InfoError::SeekHeadElement);
                    } else {
                        seek_head = Some(s);
                    }
                }
                SegmentElement::Info(i) => {
                    println!("|+ Segment information");
                    println!(
                        "| + Segment UID: {}",
                        i.segment_uid.map(format_uid).unwrap_or(String::new())
                    );
                    println!("| + Timestamp scale: {}", i.timestamp_scale);
                    if let Some(f) = i.duration {
                        let nanos = f * i.timestamp_scale as f64;
                        let d = Duration::from_nanos(nanos.round() as u64);
                        println!("| + Duration: {}", format_duration(d));
                    }
                    println!("| + Multiplexing application: {}", i.muxing_app);
                    println!("| + Writing application: {}", i.writing_app);
                    if let Some(ref date) = i.date_utc {
                        use time::format_description::well_known::Rfc3339;
                        use time::{Date, Duration};

                        let formatted = Date::from_ordinal_date(2001, 1)
                            .unwrap()
                            .midnight()
                            .assume_utc()
                            .saturating_add(Duration::nanoseconds(date.0))
                            .format(&Rfc3339)
                            .unwrap();

                        println!("| + Date: {formatted}");
                    }
                    if info.is_some() {
                        return Err(InfoError::InfoElement);
                    } else {
                        info = Some(i);
                    }
                }
                SegmentElement::Tracks(t) => {
                    //eprintln!("got tracks: {:#?}", t);
                    println!("|+ Tracks");
                    for tr in t.tracks.iter() {
                        println!("| + Track");
                        println!("|  + Track number: {}", tr.track_number);
                        println!("|  + Track UID: {}", tr.track_uid);
                        println!("|  + Track type: {}", tr.track_type);
                        println!("|  + Lacing flag: {}", tr.flag_lacing);
                        println!("|  + Default flag: {}", tr.flag_default);
                        println!("|  + Language: {}", tr.language);
                        println!("|  + Codec ID: {}", tr.codec_id);
                        println!(
                            "|  + Codec private: length {}",
                            tr.codec_private.as_ref().map(|v| v.len()).unwrap_or(0)
                        );

                        if let Some(ref v) = tr.video {
                            println!("|  + Video track");
                            println!("|    + Pixel width: {}", v.pixel_width);
                            println!("|    + Pixel height: {}", v.pixel_height);
                            println!("|    + Interlaced: {}", v.flag_interlaced);
                            if let Some(width) = v.display_width {
                                println!("|    + Display width: {}", width);
                            }
                            if let Some(height) = v.display_height {
                                println!("|    + Display height: {}", height);
                            }
                            println!("|    + Display unit: {}", v.display_unit);
                        }

                        if let Some(ref a) = tr.audio {
                            println!("|  + Audio track");
                            println!("|    + Sampling frequency: {}", a.sampling_frequency);
                            if let Some(frequency) = a.output_sampling_frequency {
                                println!("|    + Output sampling freqeuncy: {}", frequency);
                            }
                            println!("|    + Channels: {}", a.channels);
                            if let Some(bit_depth) = a.bit_depth {
                                println!("|    + Bit depth: {}", bit_depth);
                            }
                        }
                    }
                    if tracks.is_some() {
                        return Err(InfoError::TracksElement);
                    } else {
                        tracks = Some(t);
                    }
                }
                SegmentElement::Void(s) => {
                    println!("|+ EbmlVoid (size: {})", s);
                }
                el => return Err(InfoError::UnexpectedElement(format!("{:?}", el))),
            }

            b.data().offset(i)
        };
        _consumed += offset;
        b.consume(offset);
    }

    loop {
        if b.available_space() == 0 {
            b.shift();
            if b.available_space() == 0 {
                println!("buffer is already full,  cannot refill");
                break;
            }
        }

        // refill the buffer
        let sz = file.read(b.space())?;
        b.fill(sz);

        // if there's no more available data in the buffer after a write, that means we reached
        // the end of the file
        if b.available_data() == 0 {
            //panic!("no more data to read or parse, stopping the reading loop");
            break;
        }

        let offset = {
            let (i, element) = match segment_element(b.data()) {
                Ok((i, o)) => (i, o),
                Err(Err::Error(e)) | Err(Err::Failure(e)) => {
                    return Err(InfoError::Parse(format!("{:?}", e)))
                }
                Err(Err::Incomplete(_)) => continue,
            };

            match element {
                SegmentElement::SeekHead(_)
                | SegmentElement::Info(_)
                | SegmentElement::Tracks(_) => {
                    return Err(InfoError::UnexpectedElement(
                        "seek head, info or tracks element".to_string(),
                    ));
                }
                SegmentElement::Cluster(c) => {
                    println!("|+ Cluster");
                    println!("|+   Timestamp: {}", c.timestamp);
                    println!("|+   Position: {:?}", c.position);
                    println!("|+   Prev size: {:?}", c.prev_size);
                    println!("|+   Simple block: {} elements", c.simple_block.len());
                    println!("|+   Block group: {} elements", c.block_group.len());
                }
                SegmentElement::Void(s) => {
                    println!("|+ EbmlVoid (size: {})", s);
                }
                SegmentElement::Unknown(id, data) => {
                    return Err(InfoError::UnknownElement(_consumed, id, data))
                }
            }

            b.data().offset(i)
        };
        _consumed += offset;
        b.consume(offset);
    }

    Ok(())
}

fn format_uid(uid: Uuid) -> String {
    uid.as_bytes()
        .iter()
        .map(|b| format!("{b:#x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_duration(dur: Duration) -> String {
    let hrs = dur.as_secs() / 3600;
    let mins = (dur.as_secs() % 3600) / 60;
    let secs = dur.as_secs_f64() % 60.0;
    format!("{hrs:02}:{mins:02}:{secs:02.9}")
}
