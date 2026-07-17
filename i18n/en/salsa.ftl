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

## Observe page

observe-maintenance-mode = This telescope is currently in maintenance mode.
observe-wind-warning = Strong wind warning: wind speed is above the safe limit. Please stow the telescope and return when the storm has ended. You may still observe, but do so at your own risk.
observe-guest-session = Guest session
observe-guest-ends-in = ends in
observe-guest-tail = or sooner if a registered user books this telescope. Data is not saved.
observe-target = Target
observe-coord-galactic = Galactic
observe-coord-equatorial = Equatorial (J2000)
observe-coord-horizontal = Horizontal
observe-coord-sun = Sun
observe-coord-gnss = GNSS
observe-coord-stow = Stow
observe-lbl-long = Long. [deg]
observe-lbl-lat = Lat. [deg]
observe-lbl-ra = R.A. [deg]
observe-lbl-dec = Dec. [deg]
observe-lbl-az = Az. [deg]
observe-lbl-el = El. [deg]
observe-satellite = Satellite
observe-loading = Loading...
observe-track = Track
observe-stop = Stop
observe-adv-tracking = Advanced tracking settings
observe-az-offset = Az. offset [deg]
observe-el-offset = El. offset [deg]
observe-min-elevation = Minimum elevation:
observe-recommended = recommended
observe-begin = Begin
observe-end = End
observe-adv-receiver = Advanced receiver settings
observe-integration-time = Integration time
observe-interactive-end = Interactive end
observe-fixed = Fixed:
observe-seconds = seconds
observe-mode = Mode
observe-freq-switched = Freq. switched
observe-raw = Raw
observe-center-freq = Center freq. [MHz]
observe-ref-freq = Ref. freq. [MHz]
observe-freq-range = Frequency range
observe-bandwidth = Bandwidth
observe-spectral-channels = Spectral channels
observe-rfi-filter = RFI filter (sliding MAD-σ)
observe-enabled = Enabled
observe-disabled = Disabled
observe-live-spectrum = Live spectrum
observe-live-webcam = Live Webcam
observe-webcam-alt = Live webcam feed of the SALSA telescope
observe-webcam-note = Updated every second. No lights — telescopes are not visible after dark but can still be controlled.

## Observe page — strings emitted inside JS string literals. Keep them
## free of quotes and apostrophes.

observe-js-time-left = Time left:
observe-js-booking-ending = Booking ending soon, please stow telescope or extend booking.
observe-js-khz-channel = kHz/channel
observe-js-hz-channel = Hz/channel
observe-js-integrating = Integrating:
observe-js-integration-time = Integration time:
observe-js-avg-power = Avg. power:
observe-js-stopped-1 = Integration stopped: the telescope moved off the target. The data collected so far has been saved to your
observe-js-archive = observation archive
observe-js-shortly = shortly
observe-js-unless = unless you keep using the controls below
observe-js-max-session = (maximum session length reached)

## Observe errors (routes/observe.rs)

observe-error-select-satellite = Please select a satellite.
observe-error-invalid-coords = Please enter valid coordinates.
observe-error-elevation-range = Target is out of elevation range ({ $min }–{ $max }°).
observe-error-not-tracking = Telescope is not tracking. Please wait until it has reached the target.
observe-error-receiver-unreachable = Receiver is not reachable. Check the receiver address and network connection.
observe-error-center-freq = Center frequency must be between { $min } and { $max } MHz.
observe-error-ref-freq = Reference frequency must be between { $min } and { $max } MHz.
observe-error-gain = Gain must be between { $min } and { $max } dB.

## Telescope status fragment

state-telescope = Telescope
state-idle = Idle
state-slewing = Slewing
state-tracking = Tracking
state-offline = Offline
state-offline-error = Cannot connect to telescope controller.
state-low-elevation-1 = Low elevation: the telescope is pointed only
state-low-elevation-2 = above the horizon — noise from the ground and surrounding buildings may degrade the spectrum.
state-error-elevation-range = target is out of elevation range ({ $min }–{ $max }°)
state-error-io = io error in communication with telescope
state-error-not-connected = telescope is not connected
state-error-receiver = receiver failed: { $msg }

## Observe landing / no-booking / maintenance pages

observe-landing-none = No active bookings right now.
observe-landing-go-1 = Go to the
observe-landing-bookings = Bookings
observe-landing-go-2 = page to book time on a telescope, then return here when your slot is active.
observe-landing-active = You have active bookings. Select an option below:
observe-landing-with = Observe with
observe-landing-point = Point the telescope, set a target, and record a spectrum.
observe-landing-interferometry = Interferometry
observe-landing-inter-desc = Use two telescopes simultaneously to measure visibility — amplitude and phase — as a function of baseline and frequency.
observe-nobooking-text = Telescope control is not available without a valid booking.
observe-nobooking-link = Go to bookings
observe-maint-heading-1 = Apologies, but telescope
observe-maint-heading-2 = is currently in maintenance mode.
observe-maint-text-1 = It is not available for observing. Check
observe-maint-bookings = bookings
observe-maint-text-2 = to see if another telescope is free. For assistance, please contact
observe-maint-support = support

## Observations archive page. The spectrum-chart controls (Pick ranges,
## Show frequency, Log scale, the chart hint) are rewritten at runtime by
## assets/observation_chart.js and stay English until that is translated.

obs-tab-single = Single-dish
obs-inter-sessions = Interferometry sessions
obs-inter-none = No interferometry sessions yet.
obs-session-singular = session
obs-sessions-plural = sessions
obs-ended = ended
obs-no-end-time = no end time
obs-inter-delete-confirm = Delete this interferometry session and all its visibility data?
obs-my = My observations
obs-none = No observations yet.
obs-list-hint = observations · click to view spectrum · ✕ to delete selected
obs-delete-confirm = Delete this observation?
obs-spectrum = Spectrum
obs-save-png = Save PNG
obs-save-csv = Save CSV
obs-save-fits = Save FITS
obs-export-note = PNG and CSV export the corrected spectrum · FITS always exports raw data
obs-analysis = Analysis
obs-reset-all = Reset all
obs-analysis-note = Analysis is session-only and not saved — take notes of any important findings.
obs-baseline = Baseline
obs-order = Order:
obs-order-linear = 1 (linear)
obs-order-quadratic = 2 (quadratic)
obs-fit = Fit
obs-subtract = Subtract
obs-clear = Clear
obs-gaussian = Gaussian fit

## Live status page and fragments

live-webcam-note = Webcam updated every 2 seconds, timestamp is Swedish summer time (CEST = UTC+2). No lights, so telescopes are not visible after dark, but can still be controlled.
live-webcam-alt = Live webcam feed of the SALSA telescopes (Torre, Vale, Brage)
live-telescope-status = Telescope status
live-guest = Guest
live-position-unavailable = Position unavailable
live-controller = Controller
live-receiver = Receiver
webcam-disabled = Webcam disabled
webcam-offline = Webcam offline — last image { $age }
webcam-updated = Updated { $age }
webcam-unavailable = Webcam unavailable — no image from camera. Please contact support if this problem persists.
age-secs = { $n }s ago
age-mins = { $n }min ago

## Weather fragment. The -js- variants are filled in by page JS with %n%.

weather-heading = Weather at Onsala
weather-temp = Temp
weather-pressure = Press.
weather-humidity = Humid.
weather-wind = Wind
weather-10min-avg = (10min avg)
weather-gust = Gust
weather-lull = lull
weather-gust-suffix = m/s (3s max/min over 10min)
weather-js-age-secs = %n%s ago
weather-js-age-mins = %n%min ago

## Target visibility page

vis-back = Support & documentation
vis-heading = Target visibility
vis-intro-1 = For a given target and date, see when it is above the horizon at the SALSA site (Onsala, Sweden) and therefore observable. The chart shows elevation across the chosen day, with times in
vis-intro-2 = recommended lower elevation limit set by the ground and surrounding buildings.
vis-coord-system = Coordinate system
vis-coord-equatorial = Equatorial
vis-date = Date
vis-compute = Compute
vis-hover-hint = Hover or tap the chart to read off time and elevation.
vis-lbl-glon = Galactic longitude (°)
vis-lbl-glat = Galactic latitude (°)
vis-lbl-ra = Right ascension (°)
vis-lbl-dec = Declination (°)
vis-error-date = Invalid date — use YYYY-MM-DD.
vis-error-coord = Invalid coordinate system.
vis-target-galactic = Galactic { $x }°, { $y }°
vis-target-equatorial = Equatorial { $x }°, { $y }°
vis-title = { $target } on { $date } ({ $tz })
vis-not-above = Not above { $threshold }° at any time. Peak { $max }° at { $peak }.
vis-above = Above { $threshold }° from { $windows }. Max { $max }° at { $peak }.
vis-window-range = { $from } to { $to }
vis-window-join = , and{" "}
vis-axis-utc = UTC time
vis-axis-local = Local time ({ $tz })

## Login page

login-same-method-hint = Always use the same login method to keep access to your bookings and observations.
login-local-summary = Local login (exceptional cases only)
login-local-help = Local accounts are reserved for demonstrations and special cases. Contact SALSA support to request access.
login-invalid-credentials = Invalid username or password.
login-rate-limited = Too many failed attempts. Please try again later.
login-username = Username
login-password = Password
login-submit = Log in
