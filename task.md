Read the design at @docs/design.md and implement.

Use Rust for the server; see @Cargo.toml for dependencies, but add new ones as needed.

Use Python for integration test scripting. Manage the project with `uv`

# Testing

- Use unit tests and red-green TDD (implement stubs so the red step isn't just an import error)
- Use property-based tests for any parsers

Also include an integration tests. This should be a program that

- Starts the server, including configuration
- Hits the GET endpoint to get a signed nonce
- Does the PoW and constructs a formdata
- Hits the POST endpoint
- Validates that the data was saved (by checking the db) and that the submit callback ran (e.g. echo a random value to a file with it and check that it matches)

Integration tests should be implemented with `pytest` and the stdlib.


# Client

For the example client, a similar Playwright integration test should test it in a real browser. A minimal HTTP server that serves a test page and reverse-proxies GET/POST requests to `sloos` server will be required.
