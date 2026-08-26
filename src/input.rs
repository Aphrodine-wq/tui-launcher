use gilrs::{Axis, Button, EventType, Gilrs};

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
                }
                EventType::Disconnected => {
                    self.name = None;
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
        output
    }

    pub fn set_bindings(&mut self, bindings: ControllerBindings) {
        self.bindings = bindings;
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
