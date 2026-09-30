use clap::Parser;
use dokploy_cli::{cli::Cli, execute_offline};

struct CompletionContract {
    shell: &'static str,
    signature: &'static str,
}

const COMPLETION_CONTRACTS: &[CompletionContract] = &[
    CompletionContract {
        shell: "bash",
        signature: "_dokploy()",
    },
    CompletionContract {
        shell: "elvish",
        signature: "edit:completion:arg-completer[dokploy]",
    },
    CompletionContract {
        shell: "fish",
        signature: "complete -c dokploy",
    },
    CompletionContract {
        shell: "powershell",
        signature: "Register-ArgumentCompleter -Native -CommandName 'dokploy'",
    },
    CompletionContract {
        shell: "zsh",
        signature: "#compdef dokploy",
    },
];

#[test]
fn every_supported_shell_receives_the_complete_public_command_tree() {
    for contract in COMPLETION_CONTRACTS {
        let first = generate(contract.shell);
        let second = generate(contract.shell);

        assert_eq!(
            first, second,
            "{} output must be deterministic",
            contract.shell
        );
        assert!(
            first.contains(contract.signature),
            "{} output is missing its shell signature",
            contract.shell
        );

        for command in [
            "apply",
            "completions",
            "destroy",
            "import",
            "plan",
            "recover",
            "state",
        ] {
            assert!(
                first.contains(command),
                "{} output is missing the `{command}` command",
                contract.shell
            );
        }

        for option in ["auto-approve", "detailed-exitcode", "parallelism"] {
            assert!(
                first.contains(option),
                "{} output is missing the `{option}` option",
                contract.shell
            );
        }
    }
}

fn generate(shell: &str) -> String {
    let cli = Cli::try_parse_from(["dokploy", "completions", shell])
        .expect("the documented completion shell must parse");
    assert!(cli.is_offline());

    let mut output = Vec::new();
    execute_offline(cli, &mut output, false)
        .expect("completion generation must not require a terminal or connection");

    String::from_utf8(output).expect("completion scripts must be UTF-8")
}
