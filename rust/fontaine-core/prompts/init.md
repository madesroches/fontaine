{{task}}

Read the codebase before you write: the build and package files, the top-level layout, the README and any existing contributor or agent documentation, the CI configuration, and enough source to see how the code is organized. Then write the file with these parts, in this order:

- The commands to build, test and lint, including how to run one test, taken from the project's own scripts and CI rather than guessed.
- The layout: what each top-level directory or crate is for, and how the parts depend on each other.
- The conventions a newcomer would not discover on their own: error handling, naming, testing rules, anything the code enforces that is not obvious from reading one file.
- Hard rules the project states, such as security properties or things never to do.

Keep the file under about 200 lines. Prefer short bullets to prose. Do not invent rules or commands you did not find evidence for, do not list every file, and do not repeat what a README already says; point to it instead.
