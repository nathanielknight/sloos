This is a deliberately minimal form submission endpoint with proof-of-work based abuse protection. It's called `sloos`.

It's intended to be embedded in a larger site and run behind a reverse-proxy (which should do domain verification and, if necessary, rate limiting). "Forms" are created by putting HTML and JavaScript on a webpage, and are beyond the scope of this service.

# Endpoints

- GET returns a nonce, including difficulty and expiration time (as a JSON object); the server should save nonces in the database until they expire
- POST accepts the nonce, hash-cash style proof-of-work, and  any additional data from the forms (as formdata, to minimize javascript). It should be in `application/x-www-form-urlencoded` format.

POSTs check that the nonce exists, isn't expired, and that the proof-of-work is valid; the nonce should be marked as used in the database, and not accepted again (to prevent replay).Submissions are saved in the db.

An optional, configurable, **static** shell command is run to do things like receipt notification. Submission data isn't sent to this command to avoid the risk of command injection.

Nonce and proof-of-work should be in fields called `_sloos_nonce` and `_sloos_pow`, respectively.

Nonce should be 16 bytes. Use hex encoding for nonce and proof-of-work.


# Database

- SQLite via rusqlite
- Prefer nullable timestamps to booleans (e.g. `used_at` rather than `used`)
- Expired nonces should be pruned by a recurring server task (e.g. every 15 minutes)
- Tables should be separate (to avoid foreign key problems when nonces are pruned)


# Configuration

Via environment variables.

- SLOOS_DB_PATH: path to SQLite database
- SLOOS_POW_DIFFICULTY: hashcash difficulty
- SLOOS_NONCE_EXPIRATION_SECONDS: seconds until nonce expiration
- SLOOS_SUBMIT_CALLBACK: static shell command to run when new submissions are accepted

The database should be initialized if isn't already. Store `schema.sql` in a separate file and import it with `include_string!`.


# Client

The documentation should include a zero-dependency, vanilla JS client library that takes an API endpoint and a selector query and:

- disables submission
- for each matching form, hits the GET endpoint to get a nonce
- creates a proof-of-work
- adds or populates the relevant hidden fields in the form
- re-enables submission
