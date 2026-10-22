//! Dock-panel boilerplate shared by every panel.

#[macro_export]
macro_rules! dock_panel {
    ($ty:ty, $name:literal, $title:literal) => {
        impl gpui_kit::component::dock::BasePanel for $ty {
            fn panel_name(&self) -> &'static str {
                $name
            }
            // closing would lose the panel; View → toggle hides docks instead
            fn closable(&self, _: &gpui_kit::App) -> bool {
                false
            }
        }
        impl gpui_kit::component::dock::Panel for $ty {
            fn title(&mut self, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<Self>) -> impl gpui_kit::IntoElement {
                $title
            }
        }
        impl gpui_kit::EventEmitter<gpui_kit::component::dock::PanelEvent> for $ty {}
        impl gpui_kit::Focusable for $ty {
            fn focus_handle(&self, _: &gpui_kit::App) -> gpui_kit::FocusHandle {
                self.focus.clone()
            }
        }
    };
}
