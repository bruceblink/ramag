use std::rc::Rc;

use gpui::{
    Anchor, Context, DismissEvent, ElementId, Entity, Focusable as _, InteractiveElement,
    IntoElement, ParentElement as _, RenderOnce, SharedString, StyleRefinement, Styled, Window,
    div,
};
use gpui_component::{Selectable, menu::PopupMenu, popover::Popover};

type MenuBuilder = dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu;

/// 为下拉菜单及其菜单项提供统一的手型光标。
pub trait PointerDropdownMenu:
    Styled + Selectable + InteractiveElement + IntoElement + 'static
{
    fn pointer_dropdown_menu(
        self,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> PointerDropdownMenuPopover<Self> {
        self.pointer_dropdown_menu_with_anchor(Anchor::TopLeft, builder)
    }

    fn pointer_dropdown_menu_with_anchor(
        mut self,
        anchor: impl Into<Anchor>,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> PointerDropdownMenuPopover<Self> {
        let style = self.style().clone();
        let id = self.interactivity().element_id.clone().unwrap_or(0.into());
        PointerDropdownMenuPopover {
            id: SharedString::from(format!("pointer-dropdown-menu:{id:?}")).into(),
            style,
            anchor: anchor.into(),
            trigger: self,
            builder: Rc::new(builder),
        }
    }
}

impl<T> PointerDropdownMenu for T where
    T: Styled + Selectable + InteractiveElement + IntoElement + 'static
{
}

#[derive(IntoElement)]
pub struct PointerDropdownMenuPopover<T: Selectable + IntoElement + 'static> {
    id: ElementId,
    style: StyleRefinement,
    anchor: Anchor,
    trigger: T,
    builder: Rc<MenuBuilder>,
}

#[derive(Default)]
struct DropdownMenuState {
    menu: Option<Entity<PopupMenu>>,
}

impl<T> RenderOnce for PointerDropdownMenuPopover<T>
where
    T: Selectable + IntoElement + 'static,
{
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let builder = self.builder.clone();
        let menu_state =
            window.use_keyed_state(self.id.clone(), cx, |_, _| DropdownMenuState::default());

        Popover::new(SharedString::from(format!("popover:{}", self.id)))
            .appearance(false)
            .overlay_closable(false)
            .trigger(self.trigger)
            .trigger_style(self.style)
            .anchor(self.anchor)
            .content(move |_, window, cx| {
                let menu = match menu_state.read(cx).menu.clone() {
                    Some(menu) => menu,
                    None => {
                        let builder = builder.clone();
                        let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
                            builder(menu, window, cx)
                        });
                        menu_state.update(cx, |state, _| state.menu = Some(menu.clone()));
                        menu.focus_handle(cx).focus(window, cx);

                        let popover_state = cx.entity();
                        window
                            .subscribe(&menu, cx, {
                                let menu_state = menu_state.clone();
                                move |_, _: &DismissEvent, window, cx| {
                                    popover_state.update(cx, |state, cx| {
                                        state.dismiss(window, cx);
                                    });
                                    menu_state.update(cx, |state, _| state.menu = None);
                                }
                            })
                            .detach();
                        menu
                    }
                };

                div().cursor_pointer().child(menu)
            })
    }
}
