#![cfg_attr(debug_assertions, allow(dead_code, unused))]
#![cfg_attr(feature = "cargo-clippy", deny(clippy))]
#![cfg_attr(feature = "cargo-clippy", warn(clippy_pedantic))]

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::slice::Iter;

use failure::Error;
use indexmap::{self, IndexMap, IndexSet};
use memchr::Memchr;

pub mod args;
use crate::args::OpName;

type TextVec = Vec<u8>;
type TextSlice = [u8];
type LineIterator<'a> = Box<dyn Iterator<Item = &'a TextSlice> + 'a>;

type UnionSet = IndexSet<TextVec>;
type BoolMapForSet = IndexMap<TextVec, bool>;

#[derive(Default)]
struct SingleSet(BoolMapForSet);

#[derive(Default)]
struct MultipleSet(BoolMapForSet);

type SliceSet<'data> = IndexSet<&'data TextSlice>;

#[derive(Default)]
struct DiffSet<'data>(SliceSet<'data>);

#[derive(Default)]
struct IntersectSet<'data>(SliceSet<'data>);

pub type SetOpResult = Result<(), Error>;
/// Calculates and prints the set operation named by `op`. Each file in `files`
/// is treated as a set of lines:
///
/// * `union` prints the lines that occur in any file,
/// * `intersect` prints the lines that occur in all files,
/// * `diff` prints the lines that occur in the first file and no other,
/// * `single` prints the lines that occur in exactly one file, and
/// * `multiple` prints the lines that occur in more than one file.
pub fn do_calculation(op: OpName, files: &[PathBuf]) -> SetOpResult {
    use std::mem::drop;
    let mut paths = files.iter();
    let text = match paths.next() {
        None => return Ok(()),
        Some(path) => fs::read(path)?,
    };
    match op {
        OpName::Intersect => calculate_and_print(&mut IntersectSet::init(&text), paths)?,
        OpName::Diff => calculate_and_print(&mut DiffSet::init(&text), paths)?,
        OpName::Union => {
            let mut set = UnionSet::init(&text);
            drop(text);
            calculate_and_print(&mut set, paths)?;
        }
        OpName::Single => {
            let mut set = SingleSet::init(&text);
            drop(text);
            calculate_and_print(&mut set, paths)?;
        }
        OpName::Multiple => {
            let mut set = MultipleSet::init(&text);
            drop(text);
            calculate_and_print(&mut set, paths)?;
        }
    }
    Ok(())
}

fn calculate_and_print(set: &mut impl SetExpression, files: Iter<PathBuf>) -> SetOpResult {
    for f in files {
        set.operate(&fs::read(f)?);
    }
    set.finish();
    let stdout_for_locking = io::stdout();
    let mut stdout = stdout_for_locking.lock();
    for line in set.iter() {
        stdout.write_all(line)?;
    }
    Ok(())
}

trait SetExpression {
    fn operate(&mut self, text: &TextSlice);
    fn finish(&mut self) {}
    fn iter(&self) -> LineIterator;
}

// Sets are implemented as variations on the `IndexMap` type, a hash that remembers
// the order in which keys were inserted, since our 'sets' are equipped with an
// ordering on the members.
//
trait LineSet<'data>: Default {
    // The only method that implementations need to define is `insert_line`
    fn insert_line(&mut self, line: &'data TextSlice);

    // The `insert_all_lines` method breaks `text` down into lines and inserts
    // each of them into `self`
    fn insert_all_lines(&mut self, text: &'data TextSlice) {
        let mut begin = 0;
        for end in Memchr::new(b'\n', text) {
            self.insert_line(&text[begin..=end]);
            begin = end + 1;
        }
        //FIXME: this leaves the last line of the file without a newline. Given that
        // fs::read allocates an extra byte at the end of the returned vector, we could
        // just add a newline there.  But that's pretty fragile!
        if begin < text.len() {
            self.insert_line(&text[begin..]);
        }
    }
    // We initialize a `LineSet` from `text` by inserting every line contained
    // in text into an empty hash.
    fn init(text: &'data TextSlice) -> Self {
        let mut set = Self::default();
        set.insert_all_lines(text);
        set
    }
}

// The simplest `LineSet` is a `SliceSet`, whose members (hash keys) are slices
// borrowed from a text string, each slice corresponding to a line.
//
impl<'data> LineSet<'data> for SliceSet<'data> {
    fn insert_line(&mut self, line: &'data TextSlice) {
        self.insert(line);
    }
}

// The next simplest set is a `UnionSet`, which we use to calculate the union
// of the lines which occur in at least one of a sequence of files. Rather than
// keep the text of all files in memory, we allocate a `TextVec` for each set member.
//
impl<'a> LineSet<'a> for UnionSet {
    fn insert_line(&mut self, line: &'a TextSlice) {
        self.insert(line.to_vec());
    }
}
impl SetExpression for UnionSet {
    fn operate(&mut self, text: &TextSlice) {
        self.insert_all_lines(&text);
    }
    fn iter(&self) -> LineIterator {
        Box::new(self.iter().map(|v| v.as_slice()))
    }
}

// We use a `SingleSet` to calculate those lines which occur in exactly one of
// the given files, and a `MultipleSet` to calculate those lines which occur in
// more than one file.  We need to remember every line we've seen, so in that
// way these set types are like `UnionSet`, but with the additional requirement
// to keep track of whether the line occurs in just one file.  So underlying a
// `SingleSet` or a `MultipleSet` is an `IndexMap` with Boolean values, `true`
// for lines that occur in only one file and `false` for lines that occur in
// multiple files.
//
// For the first operand we just set every line's value to `true`. (If a line
// occurs more than once in just one particular file, we still count it as
// occuring in a single file.)
//
// The only implementation difference between a `SingleSet` and a `MultipleSet`
// is that at the end of the calculation we retain for a `SingleSet` the keys
// with a `true` value and for a `MultipleSet` the keys with a `false` value,
// so we use a macro to distinguish the two.
//
            // Since a line that occurs more than once in a single file still
            // counts as singular, but not if occurs in multiple files, for the
            // second and subsequent operand files we first calculate a SliceSet
            // from the file's text, and then for each of the `SliceSet`'s lines
            // we either add the line to `self` with a `true` value if it's not
            // already present, or set the line's value to `false` if it is present.
            //
            // After we've processed all the operands, we keep the keys with
            // a `true` value for a `SingleSet`, and for a `MultipleSet` the
            // keys with a `false` value.

impl<'a> LineSet<'a> for SingleSet {
    fn insert_line(&mut self, line: &'a TextSlice) {
        self.0.insert(line.to_vec(), true);
    }
}
impl SetExpression for SingleSet {
    fn operate(&mut self, text: &TextSlice) {
        let other = SliceSet::init(text);
        for line in other.iter() {
            if self.0.contains_key(*line) {
                self.0.insert(line.to_vec(), false);
            } else {
                self.0.insert(line.to_vec(), true);
            }
        }
    }
    fn finish(&mut self) {
        self.0.retain(|_k, v| *v)
    }
    fn iter(&self) -> LineIterator {
        Box::new(self.0.keys().map(|k| k.as_slice()))
    }
}

impl<'a> LineSet<'a> for MultipleSet {
    fn insert_line(&mut self, line: &'a TextSlice) {
        self.0.insert(line.to_vec(), true);
    }
}
impl SetExpression for MultipleSet {
    fn operate(&mut self, text: &TextSlice) {
        let other = SliceSet::init(text);
        for line in other.iter() {
            if self.0.contains_key(*line) {
                self.0.insert(line.to_vec(), false);
            } else {
                self.0.insert(line.to_vec(), true);
            }
        }
    }
    fn finish(&mut self) {
        self.0.retain(|_k, v| ! *v)
    }
    fn iter(&self) -> LineIterator {
        Box::new(self.0.keys().map(|k| k.as_slice()))
    }
}

// For an `IntersectSet` or a `DiffSet`, all result lines will be from the
// first file operand, so we can avoid additional allocations by keeping its
// text in memory and using subslices of its text as the members of the set.

// For subsequent operands, we take a `SliceSet` `s` of the operand's text and
// (for an `IntersectSet`) keep only those lines that occur in `s` or (for a
// `DiffSet`) remove the lines that occur in `s`.
//
// Since the only difference between `IntersectSet` and `DiffSet` is whether we
// `retain` or `discard` the members of the next file. So we can use a macro
// to define the impls for `SetExpression` and `IntoLineIterator`.

/*
fn intersect(set: &mut SliceSet, other: &SliceSet) {
    set.retain(|x| other.contains(x));
}
fn difference(set: &mut SliceSet, other: &SliceSet) {
    set.retain(|x| !other.contains(x));
}
*/

impl<'data> LineSet<'data> for IntersectSet<'data> {
    fn insert_line(&mut self, line: &'data TextSlice) {
        self.0.insert(line);
    }
}
impl<'data> SetExpression for IntersectSet<'data> {
    fn operate(&mut self, text: &TextSlice) {
        let other = SliceSet::init(text);
        self.0.retain(|x| other.contains(x));
    }
    fn iter<'me>(&'me self) -> LineIterator<'me> {
        Box::new(self.0.iter().cloned())
    }
}
impl<'data> LineSet<'data> for DiffSet<'data> {
    fn insert_line(&mut self, line: &'data TextSlice) {
        self.0.insert(line);
    }
}
impl<'data> SetExpression for DiffSet<'data> {
    fn operate(&mut self, text: &TextSlice) {
        let other = SliceSet::init(text);
        self.0.retain(|x| ! other.contains(x));
    }
    fn iter<'me>(&'me self) -> LineIterator<'me> {
        Box::new(self.0.iter().cloned())
    }
}
