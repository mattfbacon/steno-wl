use std::collections::VecDeque;

use self::chars_or_bytes::CharsOrBytes;
use self::orthography::apply_orthography_rules;
use crate::dict::{Entry, EntryPart, PloverCommand, Strokes};
use crate::keys::{Key, Keys};

mod chars_or_bytes;
mod orthography;
#[cfg(test)]
mod test;

pub trait Dict {
	fn get(&self, keys: &[Keys]) -> Option<Entry>;
	fn max_strokes(&self) -> usize;
}

// ...
impl AsRef<Self> for crate::dict::Dict {
	fn as_ref(&self) -> &Self {
		self
	}
}

impl<T: AsRef<crate::dict::Dict>> Dict for T {
	fn get(&self, keys: &[Keys]) -> Option<Entry> {
		// Cheap ref-counted clone.
		self.as_ref().get(keys).cloned()
	}

	fn max_strokes(&self) -> usize {
		self.as_ref().max_strokes()
	}
}

const BACKLOG_DEPTH: usize = 1000;

#[derive(Debug, Clone, Copy)]
struct InputState {
	caps: Option<bool>,
	space: bool,
	carry_to_next: bool,
	glue: bool,
}

impl InputState {
	const INITIAL: Self = Self {
		caps: Some(true),
		space: false,
		carry_to_next: false,
		glue: false,
	};
}

#[derive(Debug)]
struct InputEvent {
	strokes: Strokes,
	text: String,
	state_before: InputState,
}

#[derive(Debug)]
pub struct Steno<D = crate::dict::Dict> {
	dict: D,
	state: InputState,
	backlog: VecDeque<InputEvent>,
}

#[derive(Debug)]
struct Action {
	entry: Entry,
	strokes: Strokes,
	/// The number of backlog entries that must be deleted before applying the entry.
	delete_before: usize,
}

#[derive(Debug)]
pub enum SpecialAction {
	Quit,
}

#[derive(Debug)]
pub struct Output {
	pub delete_words: usize,
	pub delete: CharsOrBytes,
	pub append: String,
}

impl Output {
	fn new(delete: CharsOrBytes) -> Self {
		Self {
			delete_words: 0,
			delete,
			append: String::new(),
		}
	}

	fn delete(&mut self, amount: CharsOrBytes) {
		if amount.bytes() <= self.append.len() {
			self.append.truncate(self.append.len() - amount.bytes());
		} else {
			self.delete += amount - CharsOrBytes::for_str(&self.append);
			self.append.clear();
		}
	}

	fn delete_words(&mut self, words: usize) {
		// XXX check for text in the append buffer and possible delete it by word boundaries.
		assert!(self.append.is_empty());
		self.delete_words += words;
	}

	fn append(&mut self, text: &str) {
		self.append += text;
	}

	fn replace(&mut self, old_len: CharsOrBytes, new: &str) {
		// TODO Implement common-prefix optimization to avoid deleting and retyping the same text.
		self.delete(old_len);
		self.append(new);
	}
}

fn make_numbers(mut keys: Keys) -> Option<String> {
	const NUMBERS: &[(Key, u8)] = &[
		(Key::S, b'1'),
		(Key::T, b'2'),
		(Key::P, b'3'),
		(Key::H, b'4'),
		(Key::A, b'5'),
		(Key::O, b'0'),
		(Key::F, b'6'),
		(Key::P2, b'7'),
		(Key::L, b'8'),
		(Key::T2, b'9'),
	];

	keys.remove(Key::NumberBar);

	let mut ret = Vec::new();

	for &(key, ch) in NUMBERS {
		if keys.remove(key) {
			ret.push(ch);
		}
	}

	if keys.remove(Key::E | Key::U) {
		ret.reverse();
	}

	if keys.remove(Key::D | Key::Z) {
		ret.insert(0, b'$');
		ret.extend_from_slice(b"00");
	} else {
		if keys.remove(Key::D) {
			ret.extend_from_within(..);
		}
		if keys.remove(Key::Z) {
			ret.extend_from_slice(b"00");
		}
	}

	if keys.remove(Key::K) || keys.remove(Key::B | Key::G) {
		ret.extend_from_slice(b":00");
	}

	if !keys.is_empty() {
		return None;
	}

	Some(String::from_utf8(ret).unwrap())
}

fn make_text_action(text: Box<str>, keys: Keys) -> Action {
	make_simple_action(vec![EntryPart::Verbatim(text)].into(), keys)
}

fn make_simple_action(entry: Entry, keys: Keys) -> Action {
	Action {
		entry,
		strokes: vec![keys].into(),
		delete_before: 0,
	}
}

fn make_fallback_action(keys: Keys) -> Action {
	make_text_action(keys.to_string().into(), keys)
}

impl<D: Dict> Steno<D> {
	pub fn new(dict: D) -> Self {
		Self {
			dict,
			state: InputState::INITIAL,
			backlog: VecDeque::with_capacity(BACKLOG_DEPTH),
		}
	}

	pub fn handle_keys(&mut self, keys: Keys) -> Result<Output, SpecialAction> {
		let action = self.find_action(keys);
		self.run_action(action)
	}

	fn find_action(&self, this_keys: Keys) -> Action {
		if this_keys.contains(Key::NumberBar) {
			return make_numbers(this_keys).map_or_else(
				|| make_fallback_action(this_keys),
				|text| {
					let entry = if text.bytes().all(|b| b.is_ascii_digit()) {
						vec![EntryPart::Glue, EntryPart::Verbatim(text.into())]
					} else {
						vec![EntryPart::Verbatim(text.into())]
					};
					make_simple_action(entry.into(), this_keys)
				},
			);
		}

		let max_strokes = self.dict.max_strokes();

		// As a by-reference iterator, this is cheaply cloneable, which we take advantage of.
		let events = self
			.backlog
			.range(self.backlog.len().saturating_sub(max_strokes)..);

		let mut all_strokes: Vec<Keys> = events
			.clone()
			.flat_map(|event| &event.strokes.0)
			.copied()
			.chain(std::iter::once(this_keys))
			.collect();

		let mut skip = 0;
		for (i, event) in events
			.clone()
			.map(Some)
			.chain(std::iter::once(None))
			.enumerate()
		{
			let these_events = events.clone().skip(i);
			let these_strokes = &all_strokes[skip..];
			if let Some(entry) = self.dict.get(these_strokes) {
				all_strokes.drain(..skip);
				return Action {
					entry,
					strokes: Strokes(all_strokes),
					delete_before: these_events.len(),
				};
			}
			if let Some(event) = event {
				skip += event.strokes.num_strokes();
			}
		}

		make_fallback_action(this_keys)
	}

	fn run_action(&mut self, action: Action) -> Result<Output, SpecialAction> {
		let first_removed = self.backlog.len() - action.delete_before;

		if let Some(restore) = self.backlog.get(first_removed) {
			self.state = restore.state_before;
		}

		let delete = self
			.backlog
			.range(first_removed..)
			.map(|event| CharsOrBytes::for_str(&event.text))
			.fold(CharsOrBytes::default(), std::ops::Add::add);

		self.backlog.drain(first_removed..);

		let state_before = self.state;
		let mut output = Output::new(delete);

		self.state.glue = false;

		for part in &*action.entry.0 {
			match self.run_part(part, &mut output, state_before.glue) {
				Ok(()) => {}
				Err(PloverCommand::Backspace) => {
					if let Some(prev) = self.backlog.pop_back() {
						self.state = prev.state_before;
						output.delete(CharsOrBytes::for_str(&prev.text));
					} else {
						output.delete_words(1);
					}
				}
				Err(PloverCommand::Quit) => return Err(SpecialAction::Quit),
			}
		}

		if self.backlog.len() == BACKLOG_DEPTH {
			self.backlog.pop_front();
		}

		if !output.append.is_empty() {
			self.backlog.push_back(InputEvent {
				strokes: action.strokes,
				text: output.append.clone(),
				state_before,
			});
		}

		Ok(output)
	}

	fn run_part(
		&mut self,
		part: &EntryPart,
		output: &mut Output,
		prev_was_glue: bool,
	) -> Result<(), PloverCommand> {
		match part {
			EntryPart::Verbatim(text) => {
				if self.state.space {
					output.append(" ");
				}

				let first_pos = output.append.len();
				let mut already_appended = false;

				if !self.state.space {
					let before = Some(output.append.as_str())
						.filter(|buf| !buf.is_empty())
						.or(self.backlog.back().map(|event| &*event.text))
						.unwrap_or("");
					if let Some(combined) = apply_orthography_rules(before, text) {
						output.replace(CharsOrBytes::for_str(before), &combined);
						already_appended = true;
					}
				}

				if !already_appended {
					output.append(text);
				}

				if let Some(caps) = self.state.caps {
					let first_len = output.append[first_pos..]
						.chars()
						.next()
						.map_or(0, char::len_utf8);
					let first = &mut output.append[first_pos..][..first_len];

					if caps {
						first.make_ascii_uppercase();
					} else {
						first.make_ascii_lowercase();
					}
				}

				if !std::mem::replace(&mut self.state.carry_to_next, false) {
					self.state.caps = None;
					self.state.space = true;
				}
			}
			EntryPart::SpecialPunct(punct) => {
				output.append(punct.as_str());
				self.state.space = true;
				self.state.caps = if punct.is_sentence_end() {
					Some(true)
				} else {
					None
				};
			}
			EntryPart::SetCaps(set) => {
				self.state.caps = Some(*set);
			}
			EntryPart::SetSpace(set) => {
				self.state.space = *set;
			}
			EntryPart::CarryToNext => {
				self.state.carry_to_next = true;
			}
			EntryPart::Glue => {
				if prev_was_glue {
					self.state.space = false;
					self.state.caps = None;
				}
				self.state.glue = true;
			}
			EntryPart::PloverCommand(command) => return Err(*command),
		}

		Ok(())
	}
}