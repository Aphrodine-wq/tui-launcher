use std::time::Instant;

use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use std::collections::VecDeque;

use crate::{config::ControllerBindings, model::BindingTarget};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputAction {
    PreviousMode,
    NextMode,
    PreviousItem,
    NextItem,
    Confirm,
    Back,
    Context,
    Favorite,
    Settings,
}

#[derive(Clone, Copy, Debug)]
pub struct InputEvent {
    pub action: InputAction,
    pub pressed: bool,
}

pub struct ControllerInput {
    gilrs: Option<Gilrs>,
    active: Option<GamepadId>,
    axis_x: i8,
    axis_y: i8,
    pub name: Option<String>,
    last_navigation: Instant,
    effects: VecDeque<gilrs::ff::Effect>,
    bindings: ControllerBindings,
}

impl ControllerInput {
    pub fn new(bindings: ControllerBindings) -> Self {
        let gilrs = Gilrs::new().ok();
        let mut input = Self {
            gilrs,
            active: None,
            axis_x: 0,
            axis_y: 0,
            name: None,
            last_navigation: Instant::now(),
            effects: VecDeque::new(),
            bindings,
        };
        input.refresh_name();
        input
    }

    pub fn poll(&mut self) -> Vec<InputEvent> {
        let mut output = Vec::new();
        let bindings = self.bindings.clone();
        let Some(gilrs) = &mut self.gilrs else {
            return output;
        };
        while let Some(event) = gilrs.next_event() {
            self.active = Some(event.id);
            match event.event {
                EventType::Connected => {
                    self.name = Some(gilrs.gamepad(event.id).name().to_owned());
                }
                EventType::Disconnected => {
                    if self.active == Some(event.id) {
                        self.active = None;
                        self.name = None;
                    }
                }
                EventType::ButtonPressed(button, _) => {
                    if let Some(action) = map_button(&bindings, button) {
                        output.push(InputEvent {
                            action,
                            pressed: true,
                        });
                    }
                }
                EventType::ButtonReleased(button, _) => {
                    if let Some(action) = map_button(&bindings, button) {
                        output.push(InputEvent {
                            action,
                            pressed: false,
                        });
                    }
                }
                EventType::AxisChanged(Axis::LeftStickX, value, _) => {
                    if let Some(action) = axis_action(
                        value,
                        &mut self.axis_x,
                        InputAction::PreviousMode,
                        InputAction::NextMode,
                    ) {
                        output.push(InputEvent {
                            action,
                            pressed: true,
                        });
                    }
                }
                EventType::AxisChanged(Axis::LeftStickY, value, _) => {
                    if let Some(action) = axis_action(
                        value,
                        &mut self.axis_y,
                        InputAction::NextItem,
                        InputAction::PreviousItem,
                    ) {
                        output.push(InputEvent {
                            action,
                            pressed: true,
                        });
                    }
                }
                _ => {}
            }
        }
        if !output.is_empty() {
            self.last_navigation = Instant::now();
        }
        output
    }

    pub fn glyphs(&self) -> ButtonGlyphs {
        let name = self
            .name
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("playstation")
            || name.contains("dualshock")
            || name.contains("dualsense")
            || name.contains("sony")
        {
            ButtonGlyphs {
                confirm: playstation_label(&self.bindings.confirm).to_owned(),
                back: playstation_label(&self.bindings.back).to_owned(),
                context: playstation_label(&self.bindings.context).to_owned(),
                favorite: playstation_label(&self.bindings.favorite).to_owned(),
            }
        } else if self.name.is_some() {
            ButtonGlyphs {
                confirm: xbox_label(&self.bindings.confirm).to_owned(),
                back: xbox_label(&self.bindings.back).to_owned(),
                context: xbox_label(&self.bindings.context).to_owned(),
                favorite: xbox_label(&self.bindings.favorite).to_owned(),
            }
        } else {
            ButtonGlyphs {
                confirm: "↵".into(),
                back: "Esc".into(),
                context: "C".into(),
                favorite: "F".into(),
            }
        }
    }

    pub fn set_bindings(&mut self, bindings: ControllerBindings) {
        self.bindings = bindings;
    }

    pub fn confirm_pressed(&self) -> bool {
        match (&self.gilrs, self.active) {
            (Some(gilrs), Some(id)) => parse_button(&self.bindings.confirm)
                .is_some_and(|button| gilrs.gamepad(id).is_pressed(button)),
            _ => false,
        }
    }

    pub fn rumble(&mut self, strength: f32, duration_ms: u32) {
        use gilrs::ff::{BaseEffect, BaseEffectType, EffectBuilder, Replay, Ticks};

        let (Some(gilrs), Some(id)) = (&mut self.gilrs, self.active) else {
            return;
        };
        if !gilrs.gamepad(id).is_ff_supported() {
            return;
        }
        let duration = Ticks::from_ms(duration_ms);
        let magnitude = (strength.clamp(0.0, 1.0) * 55_000.0) as u16;
        if let Ok(effect) = EffectBuilder::new()
            .add_effect(BaseEffect {
                kind: BaseEffectType::Strong { magnitude },
                scheduling: Replay {
                    play_for: duration,
                    ..Default::default()
                },
                ..Default::default()
            })
            .gamepads(&[id])
            .finish(gilrs)
        {
            let _ = effect.play();
            self.effects.push_back(effect);
            while self.effects.len() > 8 {
                self.effects.pop_front();
            }
        }
    }

    pub fn stop_rumble(&mut self) {
        for effect in self.effects.drain(..) {
            let _ = effect.stop();
        }
    }

    fn refresh_name(&mut self) {
        let Some(gilrs) = &self.gilrs else {
            return;
        };
        if let Some((id, gamepad)) = gilrs.gamepads().find(|(_, gamepad)| gamepad.is_connected()) {
            self.active = Some(id);
            self.name = Some(gamepad.name().to_owned());
        }
    }
}

impl Drop for ControllerInput {
    fn drop(&mut self) {
        self.stop_rumble();
    }
}

#[derive(Clone)]
pub struct ButtonGlyphs {
    pub confirm: String,
    pub back: String,
    pub context: String,
    pub favorite: String,
}

fn map_button(bindings: &ControllerBindings, button: Button) -> Option<InputAction> {
    let name = button_name(button)?;
    BindingTarget::ALL
        .into_iter()
        .find(|target| bindings.get(*target) == name)
        .map(binding_action)
}

fn binding_action(target: BindingTarget) -> InputAction {
    match target {
        BindingTarget::Confirm => InputAction::Confirm,
        BindingTarget::Back => InputAction::Back,
        BindingTarget::Context => InputAction::Context,
        BindingTarget::Favorite => InputAction::Favorite,
        BindingTarget::PreviousMode => InputAction::PreviousMode,
        BindingTarget::NextMode => InputAction::NextMode,
        BindingTarget::PreviousItem => InputAction::PreviousItem,
        BindingTarget::NextItem => InputAction::NextItem,
        BindingTarget::Settings => InputAction::Settings,
    }
}

fn button_name(button: Button) -> Option<&'static str> {
    match button {
        Button::South => Some("south"),
        Button::East => Some("east"),
        Button::North => Some("north"),
        Button::West => Some("west"),
        Button::DPadLeft => Some("dpad-left"),
        Button::DPadRight => Some("dpad-right"),
        Button::DPadUp => Some("dpad-up"),
        Button::DPadDown => Some("dpad-down"),
        Button::LeftTrigger => Some("left-shoulder"),
        Button::RightTrigger => Some("right-shoulder"),
        Button::Start => Some("start"),
        _ => None,
    }
}

fn parse_button(name: &str) -> Option<Button> {
    [
        Button::South,
        Button::East,
        Button::North,
        Button::West,
        Button::DPadLeft,
        Button::DPadRight,
        Button::DPadUp,
        Button::DPadDown,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Start,
    ]
    .into_iter()
    .find(|button| button_name(*button) == Some(name))
}

fn playstation_label(name: &str) -> &str {
    match name {
        "south" => "×",
        "east" => "○",
        "north" => "△",
        "west" => "□",
        "left-shoulder" => "L1",
        "right-shoulder" => "R1",
        "dpad-left" => "←",
        "dpad-right" => "→",
        "dpad-up" => "↑",
        "dpad-down" => "↓",
        "start" => "START",
        _ => "?",
    }
}

fn xbox_label(name: &str) -> &str {
    match name {
        "south" => "A",
        "east" => "B",
        "north" => "Y",
        "west" => "X",
        "left-shoulder" => "LB",
        "right-shoulder" => "RB",
        "dpad-left" => "←",
        "dpad-right" => "→",
        "dpad-up" => "↑",
        "dpad-down" => "↓",
        "start" => "MENU",
        _ => "?",
    }
}

fn axis_action(
    value: f32,
    latch: &mut i8,
    negative: InputAction,
    positive: InputAction,
) -> Option<InputAction> {
    if value.abs() < 0.35 {
        *latch = 0;
        return None;
    }
    let direction = if value < -0.7 {
        -1
    } else if value > 0.7 {
        1
    } else {
        0
    };
    if direction == 0 || direction == *latch {
        return None;
    }
    *latch = direction;
    Some(if direction < 0 { negative } else { positive })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_uses_deadzone_and_latch() {
        let mut latch = 0;
        assert_eq!(
            axis_action(0.2, &mut latch, InputAction::Back, InputAction::Confirm),
            None
        );
        assert_eq!(
            axis_action(0.9, &mut latch, InputAction::Back, InputAction::Confirm),
            Some(InputAction::Confirm)
        );
        assert_eq!(
            axis_action(0.95, &mut latch, InputAction::Back, InputAction::Confirm),
            None
        );
        assert_eq!(
            axis_action(0.0, &mut latch, InputAction::Back, InputAction::Confirm),
            None
        );
        assert_eq!(latch, 0);
    }
}
