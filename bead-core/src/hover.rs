//! 展开 / 收起的状态机，也是隐私基线的落点。
//!
//! 这里刻意不持有 `ClipboardGateway`，也没有任何内部定时器或后台任务：状态机只产出
//! `ReadClipboardOnce` 这一条 intent，由上层在收到时调用一次 `read_clipboard()`。
//! 因此「不主动监听剪贴板」不是靠某个开关去遵守，而是结构上做不到 —— 没有监听器可注册，
//! 剪贴板变化也不会变成事件流进来。默认被动是编译期属性，不是运行期约定。
//!
//! 悬停先进入 `HoverPending` 再展开，是为了让鼠标「路过」不触发读取；锁定或编辑态下
//! `hover_enter` 不产出读取 intent，避免鼠标划过把用户正在写的内容覆盖掉。

pub const DEFAULT_HOVER_DELAY_MS: u64 = 200;
pub const DEFAULT_LEAVE_GRACE_MS: u64 = 800;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardError {
    Empty,
    NonText,
    PermissionDenied(String),
}

pub trait ClipboardGateway {
    fn read_clipboard(&self) -> Result<String, ClipboardError>;
    fn read_selection(&self) -> Result<String, ClipboardError>;
    fn content_hash(&self, text: &str) -> u64 {
        crate::sync::content_hash(text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverState {
    Idle,
    HoverPending { since_ms: u64 },
    Expanded { locked: bool, editing: bool },
    Collapsing { since_ms: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverEvent {
    HoverEnter { at_ms: u64 },
    HoverLeave { at_ms: u64 },
    Tick { at_ms: u64 },
    Pin,
    Unpin,
    EditStart,
    EditEnd,
    Refresh,
    Esc { at_ms: u64 },
    OutsideClick { at_ms: u64 },
    /// 划词取到文本（工单 07）：自动展开但不触发剪贴板读取（文本由命令直接送入）。
    SelectionCaptured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverAction {
    Expand,
    ReadClipboardOnce,
    StartCollapse,
    Collapse,
    ClearDocument,
    RejectLocked,
    RejectEditing,
}

pub struct HoverStateMachine {
    state: HoverState,
    hover_delay_ms: u64,
    leave_grace_ms: u64,
}

impl HoverStateMachine {
    pub fn new(hover_delay_ms: u64, leave_grace_ms: u64) -> Self {
        Self {
            state: HoverState::Idle,
            hover_delay_ms,
            leave_grace_ms,
        }
    }

    pub fn state(&self) -> HoverState {
        self.state
    }

    pub fn transition(&mut self, event: HoverEvent) -> Vec<HoverAction> {
        match (self.state, event) {
            (HoverState::Idle, HoverEvent::HoverEnter { at_ms }) => {
                self.state = HoverState::HoverPending { since_ms: at_ms };
                vec![]
            }
            (HoverState::Collapsing { .. }, HoverEvent::HoverEnter { at_ms }) => {
                self.state = HoverState::HoverPending { since_ms: at_ms };
                vec![]
            }
            (HoverState::Expanded { locked: true, .. }, HoverEvent::HoverEnter { .. }) => {
                vec![HoverAction::RejectLocked]
            }
            (HoverState::Expanded { editing: true, .. }, HoverEvent::HoverEnter { .. }) => {
                vec![HoverAction::RejectEditing]
            }
            (HoverState::HoverPending { .. }, HoverEvent::HoverEnter { .. })
            | (HoverState::Expanded { .. }, HoverEvent::HoverEnter { .. }) => vec![],

            // 划词：从任何被动状态自动展开（无读取 intent，文本走命令送入）；
            // 锁定/编辑态不冲突（内容不被覆盖，送入由命令层拒绝）。
            (HoverState::Idle, HoverEvent::SelectionCaptured)
            | (HoverState::HoverPending { .. }, HoverEvent::SelectionCaptured)
            | (HoverState::Collapsing { .. }, HoverEvent::SelectionCaptured) => {
                self.state = HoverState::Expanded {
                    locked: false,
                    editing: false,
                };
                vec![HoverAction::Expand]
            }
            (HoverState::Expanded { .. }, HoverEvent::SelectionCaptured) => vec![],

            (HoverState::HoverPending { .. }, HoverEvent::HoverLeave { .. }) => {
                self.state = HoverState::Idle;
                vec![]
            }
            (
                HoverState::Expanded {
                    locked: false,
                    editing: false,
                },
                HoverEvent::HoverLeave { at_ms },
            ) => {
                self.state = HoverState::Collapsing { since_ms: at_ms };
                vec![HoverAction::StartCollapse]
            }
            (HoverState::Expanded { .. }, HoverEvent::HoverLeave { .. })
            | (HoverState::Collapsing { .. }, HoverEvent::HoverLeave { .. })
            | (HoverState::Idle, HoverEvent::HoverLeave { .. }) => vec![],

            (HoverState::HoverPending { since_ms }, HoverEvent::Tick { at_ms })
                if at_ms.saturating_sub(since_ms) >= self.hover_delay_ms =>
            {
                self.state = HoverState::Expanded {
                    locked: false,
                    editing: false,
                };
                vec![HoverAction::Expand, HoverAction::ReadClipboardOnce]
            }
            (HoverState::Collapsing { since_ms }, HoverEvent::Tick { at_ms })
                if at_ms.saturating_sub(since_ms) >= self.leave_grace_ms =>
            {
                self.state = HoverState::Idle;
                vec![HoverAction::Collapse, HoverAction::ClearDocument]
            }
            (_, HoverEvent::Tick { .. }) => vec![],

            (HoverState::Expanded { editing, .. }, HoverEvent::Pin) => {
                self.state = HoverState::Expanded {
                    locked: true,
                    editing,
                };
                vec![]
            }
            (HoverState::Expanded { editing, .. }, HoverEvent::Unpin) => {
                self.state = HoverState::Expanded {
                    locked: false,
                    editing,
                };
                vec![]
            }
            (HoverState::Expanded { locked, .. }, HoverEvent::EditStart) => {
                self.state = HoverState::Expanded {
                    locked,
                    editing: true,
                };
                vec![]
            }
            (HoverState::Expanded { locked, .. }, HoverEvent::EditEnd) => {
                self.state = HoverState::Expanded {
                    locked,
                    editing: false,
                };
                vec![]
            }

            (HoverState::Expanded { .. }, HoverEvent::Refresh) => {
                vec![HoverAction::ReadClipboardOnce]
            }
            (
                HoverState::Expanded {
                    locked: false,
                    ..
                },
                HoverEvent::Esc { .. } | HoverEvent::OutsideClick { .. },
            ) => {
                self.state = HoverState::Idle;
                vec![HoverAction::Collapse, HoverAction::ClearDocument]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct CountingClipboard {
        reads: Cell<usize>,
    }

    impl CountingClipboard {
        fn new() -> Self {
            Self {
                reads: Cell::new(0),
            }
        }
    }

    impl ClipboardGateway for CountingClipboard {
        fn read_clipboard(&self) -> Result<String, ClipboardError> {
            self.reads.set(self.reads.get() + 1);
            Ok("clipboard text".into())
        }

        fn read_selection(&self) -> Result<String, ClipboardError> {
            Err(ClipboardError::PermissionDenied("not probed in ticket 01".into()))
        }
    }

    /// 上层只在收到 ReadClipboardOnce 时读一次；状态机本身拿不到 gateway。
    fn consume(actions: Vec<HoverAction>, clipboard: &CountingClipboard) {
        for action in actions {
            if action == HoverAction::ReadClipboardOnce {
                let _ = clipboard.read_clipboard();
            }
        }
    }

    fn machine() -> HoverStateMachine {
        HoverStateMachine::new(DEFAULT_HOVER_DELAY_MS, DEFAULT_LEAVE_GRACE_MS)
    }

    fn expand(machine: &mut HoverStateMachine) -> Vec<HoverAction> {
        let enter = machine.transition(HoverEvent::HoverEnter { at_ms: 0 });
        let tick = machine.transition(HoverEvent::Tick { at_ms: 200 });
        enter.into_iter().chain(tick).collect()
    }

    #[test]
    fn idle_moves_to_hover_pending_on_hover_enter() {
        let mut machine = machine();
        machine.transition(HoverEvent::HoverEnter { at_ms: 10 });
        assert_eq!(machine.state(), HoverState::HoverPending { since_ms: 10 });
    }

    #[test]
    fn hover_pending_expands_and_reads_clipboard_once_after_the_debounce() {
        let mut machine = machine();
        let clipboard = CountingClipboard::new();
        let actions = expand(&mut machine);

        consume(actions, &clipboard);
        assert_eq!(machine.state(), HoverState::Expanded { locked: false, editing: false });
        assert_eq!(clipboard.reads.get(), 1);
    }

    #[test]
    fn a_hover_shorter_than_the_debounce_never_reads_the_clipboard() {
        let mut machine = machine();
        let clipboard = CountingClipboard::new();
        let mut actions = machine.transition(HoverEvent::HoverEnter { at_ms: 0 });
        actions.extend(machine.transition(HoverEvent::Tick { at_ms: 199 }));
        actions.extend(machine.transition(HoverEvent::HoverLeave { at_ms: 199 }));

        consume(actions, &clipboard);
        assert_eq!(machine.state(), HoverState::Idle);
        assert_eq!(clipboard.reads.get(), 0);
    }

    #[test]
    fn ticks_without_a_hover_enter_never_read_the_clipboard() {
        let mut machine = machine();
        let clipboard = CountingClipboard::new();
        for at_ms in [100, 1_000, 10_000] {
            consume(machine.transition(HoverEvent::Tick { at_ms }), &clipboard);
        }
        assert_eq!(clipboard.reads.get(), 0);
    }

    #[test]
    fn expanded_moves_to_collapsing_on_hover_leave() {
        let mut machine = machine();
        expand(&mut machine);
        let actions = machine.transition(HoverEvent::HoverLeave { at_ms: 500 });

        assert!(actions.contains(&HoverAction::StartCollapse));
        assert_eq!(machine.state(), HoverState::Collapsing { since_ms: 500 });
    }

    #[test]
    fn collapsing_returns_to_idle_and_clears_content_after_the_grace_period() {
        let mut machine = machine();
        expand(&mut machine);
        machine.transition(HoverEvent::HoverLeave { at_ms: 500 });
        let actions = machine.transition(HoverEvent::Tick { at_ms: 1_300 });

        assert_eq!(machine.state(), HoverState::Idle);
        assert!(actions.contains(&HoverAction::Collapse));
        assert!(actions.contains(&HoverAction::ClearDocument));
    }

    #[test]
    fn returning_during_the_grace_period_cancels_collapse() {
        let mut machine = machine();
        expand(&mut machine);
        machine.transition(HoverEvent::HoverLeave { at_ms: 500 });
        machine.transition(HoverEvent::HoverEnter { at_ms: 600 });
        machine.transition(HoverEvent::Tick { at_ms: 800 });

        assert_eq!(machine.state(), HoverState::Expanded { locked: false, editing: false });
    }

    #[test]
    fn hover_does_not_read_clipboard_when_locked() {
        let mut machine = machine();
        let clipboard = CountingClipboard::new();
        expand(&mut machine);
        machine.transition(HoverEvent::Pin);
        let actions = machine.transition(HoverEvent::HoverEnter { at_ms: 900 });

        assert!(actions.contains(&HoverAction::RejectLocked));
        consume(actions, &clipboard);
        assert_eq!(clipboard.reads.get(), 0);
    }

    #[test]
    fn hover_does_not_read_clipboard_while_editing() {
        let mut machine = machine();
        let clipboard = CountingClipboard::new();
        expand(&mut machine);
        machine.transition(HoverEvent::EditStart);
        let actions = machine.transition(HoverEvent::HoverEnter { at_ms: 900 });

        assert!(actions.contains(&HoverAction::RejectEditing));
        consume(actions, &clipboard);
        assert_eq!(clipboard.reads.get(), 0);
    }

    #[test]
    fn refresh_reads_clipboard_even_when_locked() {
        let mut machine = machine();
        let clipboard = CountingClipboard::new();
        expand(&mut machine);
        machine.transition(HoverEvent::Pin);
        let actions = machine.transition(HoverEvent::Refresh);

        assert_eq!(actions, vec![HoverAction::ReadClipboardOnce]);
        consume(actions, &clipboard);
        assert_eq!(clipboard.reads.get(), 1);
    }

    #[test]
    fn esc_and_outside_click_collapse_immediately_when_unlocked() {
        for event in [HoverEvent::Esc { at_ms: 700 }, HoverEvent::OutsideClick { at_ms: 700 }] {
            let mut machine = machine();
            expand(&mut machine);
            let actions = machine.transition(event);

            assert_eq!(machine.state(), HoverState::Idle);
            assert!(actions.contains(&HoverAction::Collapse));
            assert!(actions.contains(&HoverAction::ClearDocument));
        }
    }

    #[test]
    fn a_captured_selection_expands_from_idle_without_reading_the_clipboard() {
        let mut machine = machine();
        let actions = machine.transition(HoverEvent::SelectionCaptured);

        assert_eq!(
            machine.state(),
            HoverState::Expanded {
                locked: false,
                editing: false
            }
        );
        assert!(actions.contains(&HoverAction::Expand));
        assert!(!actions.contains(&HoverAction::ReadClipboardOnce));
    }

    #[test]
    fn a_captured_selection_during_collapse_reopens_the_board() {
        let mut machine = machine();
        expand(&mut machine);
        machine.transition(HoverEvent::HoverLeave { at_ms: 500 });
        machine.transition(HoverEvent::SelectionCaptured);

        assert_eq!(
            machine.state(),
            HoverState::Expanded {
                locked: false,
                editing: false
            }
        );
    }

    #[test]
    fn a_captured_selection_does_not_disturb_a_locked_or_editing_board() {
        let mut locked_machine = machine();
        expand(&mut locked_machine);
        locked_machine.transition(HoverEvent::Pin);
        locked_machine.transition(HoverEvent::SelectionCaptured);
        assert!(matches!(
            locked_machine.state(),
            HoverState::Expanded { locked: true, .. }
        ));

        let mut editing = machine();
        expand(&mut editing);
        editing.transition(HoverEvent::EditStart);
        editing.transition(HoverEvent::SelectionCaptured);
        assert!(matches!(
            editing.state(),
            HoverState::Expanded { editing: true, .. }
        ));
    }
}
