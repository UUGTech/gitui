use super::{
	utils::scroll_vertical::VerticalScroll, visibility_blocking,
	CommandBlocking, CommandInfo, Component, DrawableComponent,
	EventState, InputType, InspectCommitOpen, TextInputComponent,
};
use crate::{
	components::ScrollType,
	keys::{key_match, SharedKeyConfig},
	queue::{
		Action, InternalEvent, NeedsUpdate, Queue, StackablePopupOpen,
	},
	strings, try_or_popup,
	ui::{self, Size},
};
use anyhow::Result;
use asyncgit::{
	sync::{
		self,
		branch::{
			checkout_remote_branch, BranchDetails, LocalBranch,
			RemoteBranch,
		},
		checkout_branch, get_branches_info, BranchInfo, BranchType,
		CommitId, RepoPathRef, RepoState,
	},
	AsyncGitNotification,
};
use crossterm::event::Event;
use fuzzy_matcher::FuzzyMatcher;
use std::{borrow::Cow, cell::Cell, convert::TryInto};
use tui::{
	backend::Backend,
	layout::{
		Alignment, Constraint, Direction, Layout, Margin, Rect,
	},
	text::{Span, Spans, Text},
	widgets::{Block, BorderType, Borders, Clear, Paragraph, Tabs},
	Frame,
};
use ui::style::SharedTheme;
use unicode_truncate::UnicodeTruncateStr;

///
pub struct BranchListComponent {
	repo: RepoPathRef,
	branches: Vec<BranchInfo>,
	branches_filtered: Vec<(usize, Vec<usize>)>,
	local: bool,
	has_remotes: bool,
	visible: bool,
	fuzzy_find: bool,
	fuzzy_find_input: TextInputComponent,
	selection: u16,
	scroll: VerticalScroll,
	current_height: Cell<u16>,
	queue: Queue,
	theme: SharedTheme,
	key_config: SharedKeyConfig,
}

impl DrawableComponent for BranchListComponent {
	fn draw<B: Backend>(
		&self,
		f: &mut Frame<B>,
		rect: Rect,
	) -> Result<()> {
		if self.is_visible() {
			const PERCENT_SIZE: Size = Size::new(80, 50);
			const MIN_SIZE: Size = Size::new(60, 20);

			let area = ui::centered_rect(
				PERCENT_SIZE.width,
				PERCENT_SIZE.height,
				f.size(),
			);
			let area =
				ui::rect_inside(MIN_SIZE, f.size().into(), area);
			let area = area.intersection(rect);

			f.render_widget(Clear, area);

			f.render_widget(
				Block::default()
					.title(strings::title_branches())
					.border_type(BorderType::Thick)
					.borders(Borders::ALL),
				area,
			);

			let area = area.inner(&Margin {
				vertical: 1,
				horizontal: 1,
			});

			let chunks = Layout::default()
				.direction(Direction::Vertical)
				.constraints(
					[Constraint::Length(2), Constraint::Min(6)]
						.as_ref(),
				)
				.split(area);

			self.draw_tabs(f, chunks[0]);
			self.draw_list(f, chunks[1])?;
		}

		Ok(())
	}
}

impl Component for BranchListComponent {
	fn commands(
		&self,
		out: &mut Vec<CommandInfo>,
		force_all: bool,
	) -> CommandBlocking {
		if self.visible || force_all {
			if !force_all {
				out.clear();
			}

			out.push(CommandInfo::new(
				strings::commands::scroll(&self.key_config),
				true,
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::close_popup(&self.key_config),
				true,
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::commit_details_open(
					&self.key_config,
				),
				true,
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::compare_with_head(
					&self.key_config,
				),
				!self.selection_is_cur_branch(),
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::toggle_branch_popup(
					&self.key_config,
					self.local,
				),
				true,
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::select_branch_popup(
					&self.key_config,
				),
				!self.selection_is_cur_branch()
					&& self.valid_selection(),
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::open_branch_create_popup(
					&self.key_config,
				),
				true,
				self.local,
			));

			out.push(CommandInfo::new(
				strings::commands::delete_branch_popup(
					&self.key_config,
				),
				!self.selection_is_cur_branch(),
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::merge_branch_popup(
					&self.key_config,
				),
				!self.selection_is_cur_branch(),
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::branch_popup_rebase(
					&self.key_config,
				),
				!self.selection_is_cur_branch(),
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::rename_branch_popup(
					&self.key_config,
				),
				true,
				self.local,
			));

			out.push(CommandInfo::new(
				strings::commands::fetch_remotes(&self.key_config),
				self.has_remotes,
				!self.local,
			));

			out.push(CommandInfo::new(
				strings::commands::fuzzy_find(&self.key_config),
				true,
				true,
			));
		}
		visibility_blocking(self)
	}

	//TODO: cleanup
	#[allow(clippy::cognitive_complexity)]
	fn event(&mut self, ev: &Event) -> Result<EventState> {
		if !self.visible {
			return Ok(EventState::NotConsumed);
		}

		if self.fuzzy_find {
			if let Event::Key(e) = ev {
				if key_match(e, self.key_config.keys.exit_popup) {
					self.fuzzy_find = false;
					return Ok(EventState::Consumed);
				} else if key_match(e, self.key_config.keys.popup_up)
				{
					return self
						.move_selection(ScrollType::Down)
						.map(Into::into);
				} else if key_match(
					e,
					self.key_config.keys.popup_down,
				) {
					return self
						.move_selection(ScrollType::Up)
						.map(Into::into);
				}
			}
			if self.fuzzy_find_input.event(ev)?.is_consumed() {
				self.update_filter();
				return Ok(EventState::Consumed);
			}
		}

		if let Event::Key(e) = ev {
			if key_match(e, self.key_config.keys.exit_popup) {
				self.hide();
			} else if key_match(e, self.key_config.keys.move_down) {
				return self
					.move_selection(ScrollType::Up)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.move_up) {
				return self
					.move_selection(ScrollType::Down)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.page_down) {
				return self
					.move_selection(ScrollType::PageDown)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.page_up) {
				return self
					.move_selection(ScrollType::PageUp)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.home) {
				return self
					.move_selection(ScrollType::Home)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.end) {
				return self
					.move_selection(ScrollType::End)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.tab_toggle) {
				self.local = !self.local;
				self.check_remotes();
				self.update_branches()?;
			} else if key_match(e, self.key_config.keys.enter) {
				try_or_popup!(
					self,
					"switch branch error:",
					self.switch_to_selected_branch()
				);
			} else if key_match(e, self.key_config.keys.create_branch)
				&& self.local
			{
				self.queue.push(InternalEvent::CreateBranch);
			} else if key_match(e, self.key_config.keys.rename_branch)
				&& self.valid_selection()
			{
				self.rename_branch();
			} else if key_match(e, self.key_config.keys.delete_branch)
				&& !self.selection_is_cur_branch()
				&& self.valid_selection()
			{
				self.delete_branch();
			} else if key_match(e, self.key_config.keys.merge_branch)
				&& !self.selection_is_cur_branch()
				&& self.valid_selection()
			{
				try_or_popup!(
					self,
					"merge branch error:",
					self.merge_branch()
				);
			} else if key_match(e, self.key_config.keys.rebase_branch)
				&& !self.selection_is_cur_branch()
				&& self.valid_selection()
			{
				try_or_popup!(
					self,
					"rebase error:",
					self.rebase_branch()
				);
			} else if key_match(e, self.key_config.keys.move_right)
				&& self.valid_selection()
			{
				self.inspect_head_of_branch();
			} else if key_match(
				e,
				self.key_config.keys.compare_commits,
			) && self.valid_selection()
			{
				self.hide();
				if let Some(commit_id) = self.get_selected() {
					self.queue.push(InternalEvent::OpenPopup(
						StackablePopupOpen::CompareCommits(
							InspectCommitOpen::new(commit_id),
						),
					));
				}
			} else if key_match(e, self.key_config.keys.pull)
				&& !self.local && self.has_remotes
			{
				self.queue.push(InternalEvent::FetchRemotes);
			} else if key_match(
				e,
				self.key_config.keys.cmd_bar_toggle,
			) {
				//do not consume if its the more key
				return Ok(EventState::NotConsumed);
			} else if key_match(e, self.key_config.keys.fuzzy_find) {
				self.fuzzy_find = !self.fuzzy_find;
				// if self.fuzzy_find {
				// 	self.fuzzy_find_input.show()?;
				// 	self.fuzzy_find_input.focus(true);
				// } else {
				// 	self.fuzzy_find_input.focus(false);
				// 	self.fuzzy_find_input.hide();
				// }
			}
		}

		Ok(EventState::Consumed)
	}

	fn is_visible(&self) -> bool {
		self.visible
	}

	fn hide(&mut self) {
		self.visible = false;
		self.fuzzy_find_input.hide();
	}

	fn show(&mut self) -> Result<()> {
		self.fuzzy_find_input.set_text(String::new());
		self.visible = true;
		self.fuzzy_find_input.show()?;

		Ok(())
	}
}

impl BranchListComponent {
	pub fn new(
		repo: RepoPathRef,
		queue: Queue,
		theme: SharedTheme,
		key_config: SharedKeyConfig,
	) -> Self {
		let mut fuzzy_find_input = TextInputComponent::new(
			theme.clone(),
			key_config.clone(),
			"",
			"fuzzy find",
			false,
		)
		.with_input_type(InputType::Singleline);
		fuzzy_find_input.embed();

		Self {
			branches: Vec::new(),
			branches_filtered: Vec::new(),
			local: true,
			has_remotes: false,
			visible: false,
			fuzzy_find: false,
			fuzzy_find_input,
			selection: 0,
			scroll: VerticalScroll::new(),
			queue,
			theme,
			key_config,
			current_height: Cell::new(0),
			repo,
		}
	}

	///
	pub fn open(&mut self) -> Result<()> {
		self.show()?;
		self.update_branches()?;
		self.fuzzy_find = false;
		self.update_filter();

		Ok(())
	}

	fn check_remotes(&mut self) {
		if !self.local && self.visible {
			self.has_remotes =
				get_branches_info(&self.repo.borrow(), false)
					.map(|branches| !branches.is_empty())
					.unwrap_or(false);
		}
	}

	/// fetch list of branches
	pub fn update_branches(&mut self) -> Result<()> {
		if self.is_visible() {
			self.check_remotes();
			self.branches =
				get_branches_info(&self.repo.borrow(), self.local)?;
			//remove remote branch called `HEAD`
			if !self.local {
				self.branches
					.iter()
					.position(|b| b.name.ends_with("/HEAD"))
					.map(|idx| self.branches.remove(idx));
			}
			self.set_selection(self.selection)?;
		}
		Ok(())
	}

	///
	pub fn update_git(
		&mut self,
		ev: AsyncGitNotification,
	) -> Result<()> {
		if self.is_visible() && ev == AsyncGitNotification::Push {
			self.update_branches()?;
		}

		Ok(())
	}

	fn valid_selection(&self) -> bool {
		!self.branches.is_empty()
			&& !self.branches_filtered.is_empty()
	}

	fn merge_branch(&mut self) -> Result<()> {
		if let Some(branch) =
			self.branches.get(usize::from(self.selection))
		{
			sync::merge_branch(
				&self.repo.borrow(),
				&branch.name,
				self.get_branch_type(),
			)?;

			self.hide_and_switch_tab()?;
		}

		Ok(())
	}

	fn rebase_branch(&mut self) -> Result<()> {
		if let Some(branch) =
			self.branches.get(usize::from(self.selection))
		{
			sync::rebase_branch(
				&self.repo.borrow(),
				&branch.name,
				self.get_branch_type(),
			)?;

			self.hide_and_switch_tab()?;
		}

		Ok(())
	}

	fn inspect_head_of_branch(&mut self) {
		if let Some(commit_id) = self.get_selected() {
			self.hide();
			self.queue.push(InternalEvent::OpenPopup(
				StackablePopupOpen::InspectCommit(
					InspectCommitOpen::new(commit_id),
				),
			));
		}
	}

	const fn get_branch_type(&self) -> BranchType {
		if self.local {
			BranchType::Local
		} else {
			BranchType::Remote
		}
	}

	fn hide_and_switch_tab(&mut self) -> Result<()> {
		self.hide();
		self.queue.push(InternalEvent::Update(NeedsUpdate::ALL));

		if sync::repo_state(&self.repo.borrow())? != RepoState::Clean
		{
			self.queue.push(InternalEvent::TabSwitchStatus);
		}

		Ok(())
	}

	fn selection_is_cur_branch(&self) -> bool {
		if self.branches_filtered.is_empty() {
			return false;
		}
		self.branches
			.iter()
			.enumerate()
			.filter(|(index, b)| {
				b.local_details()
					.map(|details| {
						details.is_head
							// && *index == self.selection as usize
							&& *index == self.branches_filtered[self.selection as usize].0
					})
					.unwrap_or_default()
			})
			.count() > 0
	}

	fn get_selected(&self) -> Option<CommitId> {
		self.branches
			.get(usize::from(self.selection))
			.map(|b| b.top_commit)
	}

	///
	fn move_selection(&mut self, scroll: ScrollType) -> Result<bool> {
		let new_selection = match scroll {
			ScrollType::Up => self.selection.saturating_add(1),
			ScrollType::Down => self.selection.saturating_sub(1),
			ScrollType::PageDown => self
				.selection
				.saturating_add(self.current_height.get()),
			ScrollType::PageUp => self
				.selection
				.saturating_sub(self.current_height.get()),
			ScrollType::Home => 0,
			ScrollType::End => {
				let num_branches: u16 =
					self.branches_filtered.len().try_into()?;
				num_branches.saturating_sub(1)
			}
		};

		self.set_selection(new_selection)?;

		Ok(true)
	}

	fn set_selection(&mut self, selection: u16) -> Result<()> {
		let num_branches: u16 =
			self.branches_filtered.len().try_into()?;
		let num_branches = num_branches.saturating_sub(1);

		let selection = if selection > num_branches {
			num_branches
		} else {
			selection
		};

		self.selection = selection;

		Ok(())
	}

	/// Get branches to display
	fn get_text(
		&self,
		theme: &SharedTheme,
		width_available: u16,
		height: usize,
	) -> Text {
		const UPSTREAM_SYMBOL: char = '\u{2191}';
		const TRACKING_SYMBOL: char = '\u{2193}';
		const HEAD_SYMBOL: char = '*';
		const EMPTY_SYMBOL: char = ' ';
		const THREE_DOTS: &str = "...";
		const THREE_DOTS_LENGTH: usize = THREE_DOTS.len(); // "..."
		const COMMIT_HASH_LENGTH: usize = 8;
		const IS_HEAD_STAR_LENGTH: usize = 3; // "*  "

		let branch_name_length: usize =
			width_available as usize * 40 / 100;
		// commit message takes up the remaining width
		let commit_message_length: usize = (width_available as usize)
			.saturating_sub(COMMIT_HASH_LENGTH)
			.saturating_sub(branch_name_length)
			.saturating_sub(IS_HEAD_STAR_LENGTH)
			.saturating_sub(THREE_DOTS_LENGTH);
		let mut txt = Vec::new();

		let to_display: Vec<(&BranchInfo, &Vec<usize>)> = self
			.branches_filtered
			.iter()
			.skip(self.scroll.get_top())
			.map(|a| (&self.branches[a.0], &a.1))
			.take(height)
			.collect();

		for (i, (displaybranch, indices)) in
			to_display.iter().enumerate()
		{
			let mut commit_message =
				displaybranch.top_commit_message.clone();
			if commit_message.len() > commit_message_length {
				commit_message.unicode_truncate(
					commit_message_length
						.saturating_sub(THREE_DOTS_LENGTH),
				);
				commit_message += THREE_DOTS;
			}

			let mut branch_name = displaybranch.name.clone();
			if branch_name.len()
				> branch_name_length.saturating_sub(THREE_DOTS_LENGTH)
			{
				branch_name = branch_name
					.unicode_truncate(
						branch_name_length
							.saturating_sub(THREE_DOTS_LENGTH),
					)
					.0
					.to_string();
				branch_name += THREE_DOTS;
			}

			let selected = if self.branches_filtered.is_empty() {
				false
			} else {
				self.selection as usize - self.scroll.get_top() == i
			};

			let is_head = displaybranch
				.local_details()
				.map(|details| details.is_head)
				.unwrap_or_default();
			let is_head_str =
				if is_head { HEAD_SYMBOL } else { EMPTY_SYMBOL };
			let upstream_tracking_str = match displaybranch.details {
				BranchDetails::Local(LocalBranch {
					has_upstream,
					..
				}) if has_upstream => UPSTREAM_SYMBOL,
				BranchDetails::Remote(RemoteBranch {
					has_tracking,
					..
				}) if has_tracking => TRACKING_SYMBOL,
				_ => EMPTY_SYMBOL,
			};

			let span_prefix = Span::styled(
				format!("{is_head_str}{upstream_tracking_str} "),
				theme.commit_author(selected),
			);
			let span_hash = Span::styled(
				format!(
					"{} ",
					displaybranch.top_commit.get_short_string()
				),
				theme.commit_hash(selected),
			);
			let span_msg = Span::styled(
				commit_message.to_string(),
				theme.text(true, selected),
			);

			let branch_name =
				format!("{branch_name:branch_name_length$} ");
			let spans_name = branch_name
				.char_indices()
				.map(|(c_idx, c)| {
					let hit = indices.contains(&c_idx);
					Span::styled(
						Cow::from(c.to_string()),
						theme.branch(selected, is_head, hit),
					)
				})
				.collect::<Vec<_>>();

			let mut spans: Vec<Span> = Vec::new();
			spans.push(span_prefix);
			spans.extend(spans_name);
			spans.push(span_hash);
			spans.push(span_msg);

			txt.push(Spans::from(spans));
		}

		Text::from(txt)
	}

	///
	fn switch_to_selected_branch(&mut self) -> Result<()> {
		if !self.valid_selection() {
			anyhow::bail!("no valid branch selected");
		}

		let index = self.branches_filtered[self.selection as usize].0;
		if self.local {
			checkout_branch(
				&self.repo.borrow(),
				&self.branches[index].reference,
			)?;
			self.hide();
		} else {
			checkout_remote_branch(
				&self.repo.borrow(),
				&self.branches[index],
			)?;
			self.local = true;
			self.update_branches()?;
		}

		self.queue.push(InternalEvent::Update(NeedsUpdate::ALL));

		Ok(())
	}

	fn draw_tabs<B: Backend>(&self, f: &mut Frame<B>, r: Rect) {
		let tabs = [Span::raw("Local"), Span::raw("Remote")]
			.iter()
			.cloned()
			.map(Spans::from)
			.collect();

		f.render_widget(
			Tabs::new(tabs)
				.block(
					Block::default()
						.borders(Borders::BOTTOM)
						.border_style(self.theme.block(false)),
				)
				.style(self.theme.tab(false))
				.highlight_style(self.theme.tab(true))
				.divider(strings::tab_divider(&self.key_config))
				.select(if self.local { 0 } else { 1 }),
			r,
		);
	}

	fn draw_fuzzy_find_input<B: Backend>(
		&self,
		f: &mut Frame<B>,
		r: Rect,
	) -> Result<()> {
		self.fuzzy_find_input.draw(f, r)?;
		Ok(())
	}

	fn draw_list<B: Backend>(
		&self,
		f: &mut Frame<B>,
		r: Rect,
	) -> Result<()> {
		let mut r = r;
		let mut chunks = Layout::default()
			.direction(Direction::Vertical)
			.constraints(
				[Constraint::Length(3), Constraint::Min(1)].as_ref(),
			)
			.split(r);

		f.render_widget(
			Block::default()
				.border_type(BorderType::Plain)
				.border_style(self.theme.block(self.fuzzy_find))
				.borders(Borders::ALL),
			chunks[0],
		);
		f.render_widget(
			Block::default()
				.border_type(BorderType::Plain)
				.border_style(self.theme.block(!self.fuzzy_find))
				.borders(Borders::ALL),
			chunks[1],
		);
		chunks[0] = chunks[0].inner(&Margin {
			vertical: (1),
			horizontal: (1),
		});
		chunks[1] = chunks[1].inner(&Margin {
			vertical: (1),
			horizontal: (1),
		});
		r = chunks[1];
		self.draw_fuzzy_find_input(f, chunks[0])?;
		let height_in_lines = r.height as usize;
		self.current_height.set(height_in_lines.try_into()?);

		self.scroll.update(
			self.selection as usize,
			self.branches_filtered.len(),
			height_in_lines,
		);

		f.render_widget(
			Paragraph::new(self.get_text(
				&self.theme,
				r.width,
				height_in_lines,
			))
			.alignment(Alignment::Left),
			r,
		);

		r.width += 1;
		r.height += 2;
		r.y = r.y.saturating_sub(1);

		self.scroll.draw(f, r, &self.theme);

		Ok(())
	}

	fn rename_branch(&mut self) {
		let cur_branch = &self.branches[self.selection as usize];
		self.queue.push(InternalEvent::RenameBranch(
			cur_branch.reference.clone(),
			cur_branch.name.clone(),
		));
	}

	fn delete_branch(&mut self) {
		let reference =
			self.branches[self.selection as usize].reference.clone();

		self.queue.push(InternalEvent::ConfirmAction(
			if self.local {
				Action::DeleteLocalBranch(reference)
			} else {
				Action::DeleteRemoteBranch(reference)
			},
		));
	}

	fn refresh_selection(&mut self) {
		if self.selection >= self.branches_filtered.len() as u16 {
			self.selection = self.branches_filtered.len() as u16;
			self.selection = self.selection.saturating_sub(1);
		}
		if self.branches_filtered.is_empty() {
			self.selection = 0;
		}
	}

	fn update_filter(&mut self) {
		let q = self.fuzzy_find_input.get_text();
		self.branches_filtered.clear();

		if q.is_empty() {
			self.branches_filtered.extend(
				self.branches
					.iter()
					.enumerate()
					.map(|a| (a.0, Vec::new())),
			);
			return;
		}

		let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();

		let mut branches = self
			.branches
			.iter()
			.enumerate()
			.filter_map(|a| {
				matcher
					.fuzzy_indices(&a.1.name, &q)
					.map(|(score, indices)| (score, a.0, indices))
			})
			.collect::<Vec<(_, _, _)>>();

		branches.sort_by(|(score1, _, _), (score2, _, _)| {
			score2.cmp(score1)
		});

		self.branches_filtered.extend(
			branches.into_iter().map(|entry| (entry.1, entry.2)),
		);
		self.refresh_selection();
	}
}
