//! The slash commands the host runs for a device, for the picker beside the prompt field. The host
//! decides what runs; this list only saves typing, and a test in `fontaine-tui` pins it to the
//! host's own device list.

/// A command and what it does, as the picker shows them.
pub struct DeviceCommand {
    pub name: &'static str,
    pub hint: &'static str,
}

/// Every built-in command a device may run, in the order the picker lists them. Skills run too,
/// but the host names them, not this list.
pub const DEVICE_COMMANDS: &[DeviceCommand] = &[
    DeviceCommand {
        name: "btw",
        hint: "<question>",
    },
    DeviceCommand {
        name: "compact",
        hint: "[focus]",
    },
    DeviceCommand {
        name: "context",
        hint: "[all]",
    },
    DeviceCommand {
        name: "cost",
        hint: "",
    },
    DeviceCommand {
        name: "goal",
        hint: "[<condition> | clear]",
    },
    DeviceCommand {
        name: "help",
        hint: "",
    },
    DeviceCommand {
        name: "init",
        hint: "",
    },
    DeviceCommand {
        name: "recap",
        hint: "",
    },
    DeviceCommand {
        name: "rename",
        hint: "[name]",
    },
    DeviceCommand {
        name: "status",
        hint: "",
    },
];

/// The line to put in the prompt field for `command`: a command that takes an argument leaves the
/// cursor after a space.
pub fn line_for(command: &DeviceCommand) -> String {
    if command.hint.is_empty() {
        format!("/{}", command.name)
    } else {
        format!("/{} ", command.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_with_an_argument_waits_for_it() {
        let by_name = |name: &str| DEVICE_COMMANDS.iter().find(|c| c.name == name).unwrap();
        assert_eq!(line_for(by_name("cost")), "/cost");
        assert_eq!(line_for(by_name("btw")), "/btw ");
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = DEVICE_COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), DEVICE_COMMANDS.len());
    }
}
