use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Error, Write};
use std::path::Path;

pub type Line<'a> = &'a str;
pub type GroupHash = String;

/// Create or find the appropriate file based on the `group_hash` and append the line if it does not already exist.
pub fn does_line_exist_and_set(
    temp_directory: &Path,
    group_hash: &GroupHash,
    line: Line,
) -> Result<bool, Error> {
    if does_line_exist(temp_directory, group_hash, line)? {
        return Ok(true);
    }

    let file_path = temp_directory.join(group_hash);

    // append the line because it does not exist
    let mut file = OpenOptions::new()
        .write(true)
        .append(true)
        .truncate(false)
        .open(file_path.as_path())?;

    let line = format!("{}\n", line.trim_start().trim_end());
    let _ = file.write(line.as_bytes())?;

    Ok(false)
}

pub fn does_line_exist(
    temp_directory: &Path,
    group_hash: &GroupHash,
    line: Line,
) -> Result<bool, Error> {
    let file_path = temp_directory.join(group_hash);
    let file = match File::open(file_path.as_path()) {
        Ok(file) => file,
        Err(_) => File::create(file_path.as_path())?,
    };

    let mut buf = String::new();
    let mut reader = BufReader::new(&file);
    // remove potential whitespaces and \n
    let line = line.trim_start().trim_end();
    while let Ok(amount) = reader.read_line(&mut buf) {
        if amount == 0 {
            // EOF
            break;
        }

        if buf.as_str().trim_start().trim_end() == line {
            // the line already exist in the file, we can stop here
            return Ok(true);
        }

        let _ = buf.clear();
    }

    Ok(false)
}
