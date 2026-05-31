//! Component trait and Container implementation.
//! Mirrors packages/tui/src/tui.ts (Component interface, Container class)

use std::fmt;

/// A renderable UI component.
///
/// All components must implement `render` and `invalidate`.
/// `handle_input` is optional and defaults to a no-op.
pub trait Component: Send {
    /// Render the component to a list of lines for the given viewport width.
    fn render(&self, width: u16) -> Vec<String>;

    /// Handle keyboard input data. Called when the component has focus.
    fn handle_input(&mut self, _data: &str) {}

    /// Invalidate any cached rendering state.
    /// Called when the theme changes or the component needs to re-render from scratch.
    fn invalidate(&mut self);
}

/// A container that holds and renders multiple child components.
///
/// Children are rendered in insertion order, with their output concatenated
/// line-by-line. `invalidate` cascades to all children.
pub struct Container {
    children: Vec<Box<dyn Component>>,
}

impl fmt::Debug for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Container").field("child_count", &self.children.len()).finish()
    }
}

impl Container {
    /// Create a new empty container.
    pub fn new() -> Self {
        Self { children: Vec::new() }
    }

    /// Add a child component to the container.
    pub fn add(&mut self, child: impl Component + 'static) {
        self.children.push(Box::new(child));
    }

    /// Forward `handle_input` to all children in insertion order.
    pub fn handle_input_all(&mut self, data: &str) {
        for child in &mut self.children {
            child.handle_input(data);
        }
    }

    /// Return the number of children.
    pub fn child_count(&self) -> usize {
        self.children.len()
    }
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Container {
    /// Render all children in order, concatenating their output.
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    /// Invalidate all children, cascading the invalidation.
    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A simple component that returns fixed lines.
    struct TestComponent {
        lines: Vec<String>,
    }

    impl Component for TestComponent {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
        fn invalidate(&mut self) {}
    }

    #[test]
    fn test_render_collects_lines() {
        let mut container = Container::new();
        container.add(TestComponent { lines: vec!["line1".into()] });
        container.add(TestComponent { lines: vec!["line2".into()] });

        let lines = container.render(80);
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn test_empty_container_renders_nothing() {
        let container = Container::new();
        let lines = container.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_render_width_propagates() {
        let mut container = Container::new();

        struct WidthRecorder {
            captured_widths: Arc<std::sync::Mutex<Vec<u16>>>,
        }

        impl Component for WidthRecorder {
            fn render(&self, width: u16) -> Vec<String> {
                self.captured_widths.lock().unwrap().push(width);
                vec![]
            }
            fn invalidate(&mut self) {}
        }

        let widths = Arc::new(std::sync::Mutex::new(Vec::new()));
        container.add(WidthRecorder { captured_widths: widths.clone() });

        let _ = container.render(120);
        let captured = widths.lock().unwrap();
        assert_eq!(*captured, vec![120]);
    }

    #[test]
    fn test_invalidate_cascades_to_all_children() {
        let count = Arc::new(AtomicU32::new(0));

        struct CountComp(Arc<AtomicU32>);

        impl Component for CountComp {
            fn render(&self, _width: u16) -> Vec<String> {
                vec![]
            }
            fn invalidate(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut container = Container::new();
        container.add(CountComp(count.clone()));
        container.add(CountComp(count.clone()));
        container.add(CountComp(count.clone()));

        assert_eq!(count.load(Ordering::SeqCst), 0);
        container.invalidate();
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_handle_input_default_is_noop() {
        // Verify the default handle_input implementation doesn't panic.
        struct NoopComp;
        impl Component for NoopComp {
            fn render(&self, _width: u16) -> Vec<String> {
                vec![]
            }
            fn invalidate(&mut self) {}
        }

        let mut comp = NoopComp;
        comp.handle_input("hello");
        comp.handle_input("ctrl+c");
    }

    #[test]
    fn test_container_send() {
        // Verify Container is Send (it should be since Component: Send).
        fn assert_send<T: Send>(_: &T) {}
        let container = Container::new();
        assert_send(&container);
    }
}
