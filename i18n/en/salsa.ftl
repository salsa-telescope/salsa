## Navigation (header)

nav-about = About
nav-bookings = Bookings
nav-observe = Observe
nav-observations = Observation archive
nav-support = Support
nav-live = Live status
nav-admin = Admin
nav-login = Login
nav-end-session = End session
nav-account = Account
nav-logout = Log out

## Account page

account-heading = Account
account-username = Username
account-provider = Provider
account-user-id = SALSA user ID
account-type = Account type
account-type-admin = Admin
account-type-user = User
account-timezone-label = Timezone
account-timezone-help = Dates and times across SALSA — including the bookings calendar — are shown in this timezone.
account-saved = Saved ✓ — applies as you navigate.
account-language-label = Language
account-language-help = The SALSA interface is shown in this language.
account-feedback-intro = Enjoying SALSA? We'd love to hear about it — what you observed, what surprised you, what you used the data for. Drop us a line at
account-feedback-outro = Stories like that help us prioritise the time we put into the system.
account-delete = Delete account
account-delete-confirm = Are you sure? This will anonymise your account and cancel any upcoming bookings. Past bookings and observations will be retained. If you want to remove your observations, please delete them on the Observations page first.

## Welcome page

welcome-hero-title = Radio astronomy in your browser
welcome-hero-text = Learn radio astronomy with real radio telescopes available for free for anyone to use.
welcome-observe-now = Observe now
welcome-book-telescope = Book a telescope
welcome-create-account = Create an account
welcome-observe-again = Observe again
welcome-try-another = Try another telescope
welcome-telescopes-heading = The SALSA telescopes
welcome-telescopes-text = The SALSA telescopes are available for anyone who wants to try out radio astronomy. They are about 2m in diameter and are part of the Onsala Space Observatory in Sweden.
welcome-experiments-heading = Guided experiments
welcome-experiments-text = Follow guided lab experiments to map the spiral structure of the Milky Way using the 21 cm hydrogen line, track GNSS satellites, or measure the telescope beam pattern using the Sun.
welcome-read-more = Read more
welcome-telescope-photo-alt = One of the two SALSA dish antennas at Onsala Space Observatory.
welcome-spectrum-plot-alt = Plot of the 21 cm hydrogen line spectrum produced by a SALSA observation.

## Guest session banners (welcome page)

guest-error-all-busy = All telescopes are currently in use. Please try again in a few minutes, or create a free account to reserve a time slot.
guest-error-all-maintenance = All telescopes are currently in maintenance. Please try again later.
guest-error-busy = That telescope is currently booked. Please try again later, or create a free account to reserve a time slot.
guest-error-maintenance = That telescope is currently in maintenance. Please try again later.
guest-error-guest-active = Another guest is currently using that telescope. Please try again in a moment.
guest-error-rate-limited = Too many guest sessions started from your address. Please wait a few minutes, or create a free account to reserve a time slot.
guest-error-not-found = Telescope not found.
guest-error-internal = Something went wrong starting the guest session. Please try again.
guest-ended-user-heading = Session ended
guest-ended-user-message = Thanks for trying SALSA.
guest-ended-idle-heading = Session timed out
guest-ended-idle-message = Your guest session ended due to inactivity.
guest-ended-ceiling-heading = 30-minute limit reached
guest-ended-ceiling-message = Your guest session reached the maximum length for unregistered visitors.
guest-ended-preempted-heading = Telescope reserved by another user
guest-ended-preempted-message = Your guest session ended because a registered user booked this telescope.

## Bookings page

bookings-known-issue = Known issue
bookings-upcoming = Upcoming bookings
bookings-export-ics = Export to calendar (.ics)
bookings-observe = Observe
bookings-observe-opens = Observe opens at
bookings-delete = Delete
bookings-delete-confirm = Delete this booking?
bookings-weekly-title = Book telescope — weekly view
bookings-prev = Prev
bookings-today = Today
bookings-next = Next
bookings-slots-used = booking slots used
bookings-current-time = Current time:
bookings-legend-free = Free
bookings-legend-mine = My booking
bookings-legend-booked = Booked
bookings-legend-past = Past
bookings-legend-maintenance = Maintenance
bookings-help-times = All times are shown in your timezone
bookings-help-change = change it on your
bookings-help-profile = profile
bookings-help-click = Click a green slot to book it. Click a blue slot to cancel.
bookings-select-telescope = Select telescope:
bookings-slot-book = Book
bookings-under-maintenance = is under maintenance
bookings-limit-reached = Booking limit reached
bookings-mine-title = My booking — click to cancel
bookings-mine-active-title = My booking (active now) — click to cancel
bookings-booked-other = Booked by another user
bookings-dst-gap = This hour does not exist in your timezone (daylight-saving change)
bookings-until = Until:
bookings-description = Description
bookings-optional = (optional)
bookings-desc-placeholder = What are you observing?
bookings-close = Close

## Bookings page — strings emitted inside JS string literals in the
## calendar dialog. Keep them free of quotes and apostrophes.
## %tel% and %time% are placeholders filled in by the page JS.

bookings-js-book = Book
bookings-js-cancel = Cancel
bookings-js-slot = slot
bookings-js-slots = slots
bookings-js-confirm-title = Confirm booking
bookings-js-cancel-title = Cancel booking
bookings-js-book-from = Book %tel% from %time%
bookings-js-book-at = Book %tel% at %time%?
bookings-js-cancel-from = Cancel %tel% from %time%
bookings-js-cancel-at = Cancel %tel% at %time%?
bookings-js-booked-by = booked by
bookings-js-cancel-booking = Cancel booking

## Booking errors (routes/booking.rs)

booking-error-slot-ended = Cannot book a slot that has already ended.
booking-error-description-too-long = Description is too long (max { $max } characters).
booking-error-maintenance = { $telescope } is currently under maintenance.
booking-error-limit = You have reached the maximum of { $max } upcoming bookings.
booking-error-already-booked = Slot at { $time } on { $date } is already booked.

## Date format patterns (chrono strftime). Translated so word order can
## differ per language; rendered with the language's chrono locale.

fmt-week-short = %b %d
fmt-week-full = %b %d, %Y
fmt-day-col = %a %d

## Login page

login-same-method-hint = Always use the same login method to keep access to your bookings and observations.
login-local-summary = Local login (exceptional cases only)
login-local-help = Local accounts are reserved for demonstrations and special cases. Contact SALSA support to request access.
login-invalid-credentials = Invalid username or password.
login-rate-limited = Too many failed attempts. Please try again later.
login-username = Username
login-password = Password
login-submit = Log in
