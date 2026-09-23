#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSpan {
    pub file_index: usize,
    pub offset: u64,
    pub length: u64,
    pub selected: bool,
    pub padding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiecePlan {
    pub piece_index: u64,
    pub piece_offset: u64,
    pub length: u64,
    pub selected_bytes: u64,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileWriteSegment {
    pub file_index: usize,
    pub file_offset: u64,
    pub piece_data_offset: usize,
    pub length: usize,
}

pub fn selected_write_segments(
    files: &[FileSpan],
    piece_offset: u64,
    piece_length: usize,
) -> Result<Vec<FileWriteSegment>, String> {
    let piece_end = piece_offset
        .checked_add(piece_length as u64)
        .ok_or_else(|| "piece range overflow".to_owned())?;
    let mut segments = Vec::new();
    for file in files.iter().filter(|file| file.selected && !file.padding) {
        let file_end = file
            .offset
            .checked_add(file.length)
            .ok_or_else(|| "file range overflow".to_owned())?;
        let start = piece_offset.max(file.offset);
        let end = piece_end.min(file_end);
        if start >= end {
            continue;
        }
        segments.push(FileWriteSegment {
            file_index: file.file_index,
            file_offset: start - file.offset,
            piece_data_offset: (start - piece_offset)
                .try_into()
                .map_err(|_| "piece data offset exceeds platform size".to_owned())?,
            length: (end - start)
                .try_into()
                .map_err(|_| "write segment exceeds platform size".to_owned())?,
        });
    }
    Ok(segments)
}

pub fn map_selected_files_to_pieces(
    files: &[FileSpan],
    piece_length: u64,
) -> Result<Vec<PiecePlan>, String> {
    if piece_length == 0 {
        return Err("piece length must be greater than zero".into());
    }
    let total_length = files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.length)
            .ok_or_else(|| "content length overflow".to_owned())
    })?;
    let piece_count = total_length.div_ceil(piece_length);
    let mut plans = Vec::with_capacity(piece_count as usize);
    for piece_index in 0..piece_count {
        let start = piece_index * piece_length;
        let end = (start + piece_length).min(total_length);
        let mut selected_bytes = 0;
        for file in files {
            let file_start = file.offset;
            let file_end = file.offset + file.length;
            let overlap = end.min(file_end).saturating_sub(start.max(file_start));
            if file.selected && !file.padding {
                selected_bytes += overlap;
            }
        }
        plans.push(PiecePlan {
            piece_index,
            piece_offset: start,
            length: end - start,
            selected_bytes,
            required: selected_bytes > 0,
        });
    }
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn shared_boundary_piece_is_required_in_full() {
        let files = [
            FileSpan {
                file_index: 0,
                offset: 0,
                length: 6,
                selected: false,
                padding: false,
            },
            FileSpan {
                file_index: 1,
                offset: 6,
                length: 8,
                selected: true,
                padding: false,
            },
        ];
        let plans = map_selected_files_to_pieces(&files, 10).unwrap();
        assert_eq!(plans.len(), 2);
        assert!(plans[0].required);
        assert_eq!(plans[0].selected_bytes, 4);
        assert_eq!(plans[0].length, 10);
        assert!(plans[1].required);
        assert_eq!(plans[1].selected_bytes, 4);
    }

    #[test]
    fn padding_bytes_do_not_select_a_piece() {
        let files = [FileSpan {
            file_index: 0,
            offset: 0,
            length: 16,
            selected: true,
            padding: true,
        }];
        let plans = map_selected_files_to_pieces(&files, 8).unwrap();
        assert!(plans.iter().all(|piece| !piece.required));
    }

    #[test]
    fn verified_boundary_piece_writes_only_selected_file_bytes() {
        let files = [
            FileSpan {
                file_index: 0,
                offset: 0,
                length: 6,
                selected: false,
                padding: false,
            },
            FileSpan {
                file_index: 1,
                offset: 6,
                length: 8,
                selected: true,
                padding: false,
            },
        ];
        assert_eq!(
            selected_write_segments(&files, 0, 10).unwrap(),
            [FileWriteSegment {
                file_index: 1,
                file_offset: 0,
                piece_data_offset: 6,
                length: 4,
            }]
        );
        assert_eq!(
            selected_write_segments(&files, 10, 4).unwrap(),
            [FileWriteSegment {
                file_index: 1,
                file_offset: 4,
                piece_data_offset: 0,
                length: 4,
            }]
        );
    }

    proptest! {
        #[test]
        fn piece_plans_cover_content_without_overflow(lengths in proptest::collection::vec(0_u64..1_000_000, 0..64), piece_length in 1_u64..1_000_000) {
            let mut offset = 0_u64;
            let files: Vec<_> = lengths.into_iter().enumerate().map(|(index, length)| {
                let span = FileSpan { file_index: index, offset, length, selected: index % 2 == 0, padding: false };
                offset += length;
                span
            }).collect();
            let plans = map_selected_files_to_pieces(&files, piece_length).unwrap();
            prop_assert_eq!(plans.iter().map(|piece| piece.length).sum::<u64>(), offset);
            prop_assert!(plans.iter().all(|piece| piece.selected_bytes <= piece.length));
        }
    }
}
