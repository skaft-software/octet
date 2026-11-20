//! Test-side graphics plane. Text/ED/EL never erase Kitty placements.
//! Feed every frame, not only frames that contain graphics commands.
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Placement {
    pub image: u32,
    pub placement: Option<u32>,
    pub row: u16,
    pub column: u16,
    pub rows: u16,
    pub columns: u16,
}

impl Placement {
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        (self.row..self.row + self.rows).flat_map(move |row| {
            (self.column..self.column + self.columns).map(move |column| (row, column))
        })
    }
}

pub struct Kitty {
    text: vt100::Parser,
    live: Vec<Placement>,
    transmission: Option<Placement>,
    /// Image IDs whose data the terminal holds (libghostty keeps data until
    /// `d=I`, a delete-all, or a screen switch).
    data: std::collections::BTreeSet<u32>,
    /// Completed payload transmissions (`a=T`) since construction.
    pub transmissions: usize,
    /// Base64 image payload bytes received since construction, across every
    /// chunk. A frame that only moves or crops resident images adds none.
    pub payload_bytes: usize,
}

impl Kitty {
    pub fn new(rows: u16, columns: u16) -> Self {
        Self {
            text: vt100::Parser::new(rows, columns, 0),
            live: Vec::new(),
            transmission: None,
            data: std::collections::BTreeSet::new(),
            transmissions: 0,
            payload_bytes: 0,
        }
    }

    pub fn feed(&mut self, frame: &[u8]) {
        let mut remaining = frame;
        while let Some(start) = remaining.windows(3).position(|bytes| bytes == b"\x1b_G") {
            self.text_segment(&remaining[..start]);
            remaining = &remaining[start + 3..];
            let end = remaining
                .windows(2)
                .position(|bytes| bytes == b"\x1b\\")
                .expect("complete graphics command in frame");
            let command = std::str::from_utf8(&remaining[..end]).unwrap();
            let header = command.split(';').next().unwrap();
            self.payload_bytes += command.split_once(';').map_or(0, |(_, data)| data.len());
            let fields: BTreeMap<_, _> = header
                .split(',')
                .filter_map(|field| field.split_once('='))
                .collect();
            let number = |key| fields.get(key).and_then(|v| v.parse::<u32>().ok());
            match fields.get("a").copied() {
                Some("d") => match fields.get("d").copied().unwrap_or("a") {
                    "A" => {
                        self.live.clear();
                        self.data.clear();
                    }
                    "a" => self.live.clear(),
                    kind @ ("I" | "i") => {
                        let image = number("i").unwrap();
                        let placement = number("p").filter(|id| *id != 0);
                        self.live.retain(|p| {
                            p.image != image || placement.is_some_and(|id| p.placement != Some(id))
                        });
                        if kind == "I" {
                            self.data.remove(&image);
                            // kitty frees every image left with no placement
                            // when a data-freeing delete arrives.
                            let live = &self.live;
                            self.data.retain(|id| live.iter().any(|p| p.image == *id));
                        }
                    }
                    other => panic!("unmodelled deletion {other}"),
                },
                Some(action @ ("T" | "p")) => {
                    if action == "p" {
                        assert!(
                            self.data.contains(&number("i").unwrap()),
                            "a=p placed image {} whose data the terminal does not hold",
                            number("i").unwrap()
                        );
                    }
                    // All fixed-viewport producers must suppress cursor movement.
                    assert_eq!(number("C"), Some(1));
                    let (row, column) = self.text.screen().cursor_position();
                    let placement = Placement {
                        image: number("i").unwrap(),
                        placement: number("p").filter(|id| *id != 0),
                        row,
                        column,
                        rows: number("r").unwrap() as u16,
                        columns: number("c").unwrap() as u16,
                    };
                    if number("m") == Some(1) {
                        assert!(self.transmission.replace(placement).is_none());
                    } else {
                        if action == "T" {
                            self.data.insert(placement.image);
                            self.transmissions += 1;
                        }
                        self.place(placement);
                    }
                }
                None if number("m") == Some(0) => {
                    let placement = self.transmission.take().expect("chunked transmission");
                    self.data.insert(placement.image);
                    self.transmissions += 1;
                    self.place(placement);
                }
                None if number("m") == Some(1) => assert!(self.transmission.is_some()),
                other => panic!("unmodelled graphics action {other:?}"),
            }
            remaining = &remaining[end + 2..];
        }
        self.text_segment(remaining);
        if [b"\x1b[?1049h".as_slice(), b"\x1b[?1049l"]
            .iter()
            .any(|switch| frame.windows(switch.len()).any(|w| w == *switch))
        {
            // Each screen keeps its own images.
            self.data.clear();
        }
        assert!(
            self.transmission.is_none(),
            "incomplete graphics transmission"
        );
    }

    /// Text between graphics commands. A full clear frees image data in some
    /// libghostty/kitty builds; model the conservative case, in stream order.
    fn text_segment(&mut self, bytes: &[u8]) {
        if bytes.windows(4).any(|w| w == b"\x1b[2J") {
            self.data.clear();
        }
        self.text.process(bytes);
    }

    fn place(&mut self, placement: Placement) {
        // Re-uploading i=N does NOT retire anonymous placements. Only a nonzero
        // p=N replaces the matching placement, regardless of its screen row.
        if placement.placement.is_some() {
            self.live
                .retain(|old| old.image != placement.image || old.placement != placement.placement);
        }
        self.live.push(placement);
    }

    pub fn assert_placements(&self, expected: &[Placement]) {
        let mut live = self.live.clone();
        let mut expected = expected.to_vec();
        live.sort();
        expected.sort();
        assert_eq!(
            live, expected,
            "graphics plane disagrees with current anchors"
        );
        let cells = |placements: &[Placement]| {
            placements
                .iter()
                .flat_map(|p| p.cells().map(move |cell| (p.image, cell)))
                .collect::<Vec<_>>()
        };
        assert_eq!(cells(&live), cells(&expected));
    }
}
