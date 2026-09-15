use gilrs::{
    Axis, Button, EventType, GamepadId, Gilrs,
    ff::{BaseEffect, BaseEffectType, EffectBuilder, Replay, Ticks},
};
use std::time::Duration;

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
    axis_x: i8,
    axis_y: i8,
    pub name: Option<String>,
    bindings: ControllerBindings,
    /// The gamepad most recently seen connected/active.
    active: Option<GamepadId>,
    /// Raw button pressed since the last `take_pressed_button`, for rebinding.
    last_button: Option<String>,
    /// Analog-stick activation threshold; set from settings.
    deadzone: f32,
}

impl ControllerInput {
    pub fn new(bindings: ControllerBindings) -> Self {
        let gilrs = Gilrs::new().ok();
        let mut input = Self {
            gilrs,
            axis_x: 0,
            axis_y: 0,
            name: None,
            bindings,
            active: None,
            last_button: None,
            deadzone: 0.35,
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
            match event.event {
                EventType::Connected => {
                    self.name = Some(gilrs.gamepad(event.id).name().to_owned());
                    self.active = Some(event.id);
                }
                EventType::Disconnected => {
                    self.name = None;
                    if self.active == Some(event.id) {
                        self.active = None;
                    }
                }
                EventType::ButtonPressed(button, _) => {
                    self.active = Some(event.id);
                    if let Some(name) = button_name(button) {
                        self.last_button = Some(name.to_owned());
                    }
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
                    let deadzone = self.deadzone;
                    if let Some(action) = axis_action(
                        value,
                        &mut self.axis_x,
                        deadzone,
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
                    let deadzone = self.deadzone;
                    if let Some(action) = axis_action(
                        value,
                        &mut self.axis_y,
                        deadzone,
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
        output
    }

    pub fn set_bindings(&mut self, bindings: ControllerBindings) {
        self.bindings = bindings;
    }

    pub fn set_deadzone(&mut self, deadzone: f32) {
        self.deadzone = deadzone.clamp(0.05, 0.9);
    }

    /// The raw button pressed since the last call, consumed. Used to
    /// capture a button while rebinding.
    pub fn take_pressed_button(&mut self) -> Option<String> {
        self.last_button.take()
    }

    pub fn clear_pressed_button(&mut self) {
        self.last_button = None;
    }

    /// Live pressed-state of each mappable button, for the tester panel.
    pub fn button_states(&self) -> Vec<(&'static str, bool)> {
        const BUTTONS: [(&str, Button); 11] = [
            ("south", Button::South),
            ("east", Button::East),
            ("north", Button::North),
            ("west", Button::West),
            ("dpad-left", Button::DPadLeft),
            ("dpad-right", Button::DPadRight),
            ("dpad-up", Button::DPadUp),
            ("dpad-down", Button::DPadDown),
            ("left-shoulder", Button::LeftTrigger),
            ("right-shoulder", Button::RightTrigger),
            ("start", Button::Start),
        ];
        let gamepad = self
            .gilrs
            .as_ref()
            .zip(self.active)
            .map(|(gilrs, id)| gilrs.gamepad(id));
        BUTTONS
            .into_iter()
            .map(|(name, button)| (name, gamepad.is_some_and(|pad| pad.is_pressed(button))))
            .collect()
    }

    /// Fire a short rumble on the active gamepad. Best-effort: silently
    /// does nothing when force feedback is unavailable.
    pub fn rumble(&mut self, strength: f32, ms: u16) {
        let Some(gilrs) = &mut self.gilrs else {
            return;
        };
        let Some(id) = self.active else {
            return;
        };
        if !gilrs.gamepad(id).is_ff_supported() {
            return;
        }
        let magnitude = (strength.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
        let effect = BaseEffect {
            kind: BaseEffectType::Strong { magnitude },
            scheduling: Replay {
                after: Ticks::from_ms(0),
                play_for: Ticks::from_ms(u32::from(ms)),
                with_delay: Ticks::from_ms(0),
            },
            envelope: Default::default(),
        };
        if let Ok(effect) = EffectBuilder::new()
            .add_effect(effect)
            .gamepads(&[id])
            .finish(gilrs)
        {
            let _ = effect.play();
            // Keep the handle alive for the effect's duration.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(u64::from(ms) + 60));
                drop(effect);
            });
        }
    }

    fn refresh_name(&mut self) {
        let Some(gilrs) = &self.gilrs else {
            return;
        };
        if let Some((_, gamepad)) = gilrs.gamepads().find(|(_, gamepad)| gamepad.is_connected()) {
            self.name = Some(gamepad.name().to_owned());
        }
    }
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

fn axis_action(
    value: f32,
    latch: &mut i8,
    deadzone: f32,
    negative: InputAction,
    positive: InputAction,
) -> Option<InputAction> {
    if value.abs() < deadzone {
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
            axis_action(
                0.2,
                &mut latch,
                0.35,
                InputAction::Back,
                InputAction::Confirm
            ),
            None
        );
        assert_eq!(
            axis_action(
                0.9,
                &mut latch,
                0.35,
                InputAction::Back,
                InputAction::Confirm
            ),
            Some(InputAction::Confirm)
        );
        assert_eq!(
            axis_action(
                0.95,
                &mut latch,
                0.35,
                InputAction::Back,
                InputAction::Confirm
            ),
            None
        );
        assert_eq!(
            axis_action(
                0.0,
                &mut latch,
                0.35,
                InputAction::Back,
                InputAction::Confirm
            ),
            None
        );
        assert_eq!(latch, 0);
    }
}
