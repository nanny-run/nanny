# Contribute to the Nanny docs

Thank you for your interest in contributing to our documentation! This guide will help you get started.

## How to contribute

### Option 1: Edit directly on GitHub

1. Navigate to the page you want to edit
2. Click the "Edit this file" button (the pencil icon)
3. Make your changes and submit a pull request

### Option 2: Local development

1. Fork and clone [nanny-run/nanny](https://github.com/nanny-run/nanny)
2. Install the Mintlify CLI: `npm i -g mint`
3. Create a branch for your changes
4. Edit the `.mdx` files in `docs/`
5. Run `mint dev` from the `docs/` directory
6. Preview your changes at `http://localhost:3000`
7. Commit and open a pull request against `main`

## Writing guidelines

- **Use active voice**: "Run the command" not "The command should be run"
- **Address the reader directly**: Use "you" instead of "the user"
- **Keep sentences concise**: Aim for one idea per sentence
- **Lead with the goal**: Start instructions with what the user wants to accomplish
- **Use consistent terminology**: Don't alternate between synonyms for the same concept
- **Include examples**: Show, don't just tell
