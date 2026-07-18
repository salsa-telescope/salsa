## Navigation (header)

nav-about = Om SALSA
nav-bookings = Bokningar
nav-observe = Observera
nav-observations = Observationsarkiv
nav-support = Support
nav-live = Livestatus
nav-admin = Admin
nav-login = Logga in
nav-end-session = Avsluta session
nav-account = Konto
nav-logout = Logga ut

## Account page

account-heading = Konto
account-username = Användarnamn
account-provider = Inloggningstjänst
account-user-id = SALSA-användar-ID
account-type = Kontotyp
account-type-admin = Administratör
account-type-user = Användare
account-timezone-label = Tidszon
account-timezone-help = Datum och tider i hela SALSA — inklusive bokningskalendern — visas i den här tidszonen.
account-saved = Sparat ✓ — gäller när du navigerar vidare.
account-language-label = Språk
account-language-help = SALSA:s gränssnitt visas på det här språket.
account-feedback-intro = Trivs du med SALSA? Vi vill gärna höra om det — vad du observerade, vad som förvånade dig, vad du använde datan till. Skriv en rad till
account-feedback-outro = Sådana berättelser hjälper oss att prioritera tiden vi lägger på systemet.
account-delete = Radera konto
account-delete-confirm = Är du säker? Detta anonymiserar ditt konto och avbokar kommande bokningar. Tidigare bokningar och observationer behålls. Om du vill ta bort dina observationer, radera dem först på sidan Observationer.

## Welcome page

welcome-hero-title = Radioastronomi i din webbläsare
welcome-hero-text = Lär dig radioastronomi med riktiga radioteleskop, gratis för alla att använda.
welcome-observe-now = Observera nu
welcome-book-telescope = Boka ett teleskop
welcome-create-account = Skapa ett konto
welcome-observe-again = Observera igen
welcome-try-another = Prova ett annat teleskop
welcome-telescopes-heading = SALSA-teleskopen
welcome-telescopes-text = SALSA-teleskopen är tillgängliga för alla som vill prova på radioastronomi. De är cirka 2 m i diameter och ingår i Onsala rymdobservatorium i Sverige.
welcome-experiments-heading = Guidade experiment
welcome-experiments-text = Följ guidade laborationer för att kartlägga Vintergatans spiralstruktur med 21 cm-vätelinjen, följa GNSS-satelliter eller mäta teleskopets lobmönster med hjälp av solen.
welcome-read-more = Läs mer
welcome-telescope-photo-alt = En av SALSA:s två parabolantenner vid Onsala rymdobservatorium.
welcome-spectrum-plot-alt = Diagram över 21 cm-vätelinjens spektrum från en SALSA-observation.

## Guest session banners (welcome page)

guest-error-all-busy = Alla teleskop används just nu. Försök igen om några minuter, eller skapa ett gratis konto för att boka en tid.
guest-error-all-maintenance = Alla teleskop är på underhåll just nu. Försök igen senare.
guest-error-busy = Det teleskopet är bokat just nu. Försök igen senare, eller skapa ett gratis konto för att boka en tid.
guest-error-maintenance = Det teleskopet är på underhåll just nu. Försök igen senare.
guest-error-guest-active = En annan gäst använder det teleskopet just nu. Försök igen om en liten stund.
guest-error-rate-limited = För många gästsessioner har startats från din adress. Vänta några minuter, eller skapa ett gratis konto för att boka en tid.
guest-error-not-found = Teleskopet hittades inte.
guest-error-internal = Något gick fel när gästsessionen skulle startas. Försök igen.
guest-ended-user-heading = Sessionen avslutad
guest-ended-user-message = Tack för att du provade SALSA.
guest-ended-idle-heading = Sessionen avbröts
guest-ended-idle-message = Din gästsession avslutades på grund av inaktivitet.
guest-ended-ceiling-heading = 30-minutersgränsen nådd
guest-ended-ceiling-message = Din gästsession nådde maxlängden för oregistrerade besökare.
guest-ended-preempted-heading = Teleskopet reserverades av en annan användare
guest-ended-preempted-message = Din gästsession avslutades eftersom en registrerad användare bokade det här teleskopet.

## Bookings page

bookings-known-issue = Känt problem
bookings-upcoming = Kommande bokningar
bookings-export-ics = Exportera till kalender (.ics)
bookings-observe = Observera
bookings-observe-opens = Observation öppnar
bookings-delete = Radera
bookings-delete-confirm = Radera bokningen?
bookings-weekly-title = Boka teleskop — veckovy
bookings-prev = Föreg
bookings-today = Idag
bookings-next = Nästa
bookings-slots-used = bokningsbara tider använda
bookings-current-time = Aktuell tid:
bookings-legend-free = Ledig
bookings-legend-mine = Min bokning
bookings-legend-booked = Bokad
bookings-legend-past = Passerad
bookings-legend-maintenance = Underhåll
bookings-help-times = Alla tider visas i din tidszon
bookings-help-change = ändra den på din
bookings-help-profile = profil
bookings-help-click = Klicka på en grön tid för att boka. Klicka på en blå tid för att avboka.
bookings-select-telescope = Välj teleskop:
bookings-slot-book = Boka
bookings-under-maintenance = är på underhåll
bookings-limit-reached = Bokningsgränsen är nådd
bookings-mine-title = Min bokning — klicka för att avboka
bookings-mine-active-title = Min bokning (aktiv nu) — klicka för att avboka
bookings-booked-other = Bokad av en annan användare
bookings-dst-gap = Den här timmen finns inte i din tidszon (sommartidsomställning)
bookings-until = Till:
bookings-description = Beskrivning
bookings-optional = (valfritt)
bookings-desc-placeholder = Vad ska du observera?
bookings-close = Stäng

## Bookings page — strings emitted inside JS string literals in the
## calendar dialog. Keep them free of quotes and apostrophes.
## %tel% and %time% are placeholders filled in by the page JS.

bookings-js-book = Boka
bookings-js-cancel = Avboka
bookings-js-slot = tid
bookings-js-slots = tider
bookings-js-confirm-title = Bekräfta bokning
bookings-js-cancel-title = Avboka bokning
bookings-js-book-from = Boka %tel% från %time%
bookings-js-book-at = Boka %tel% %time%?
bookings-js-cancel-from = Avboka %tel% från %time%
bookings-js-cancel-at = Avboka %tel% %time%?
bookings-js-booked-by = bokad av
bookings-js-cancel-booking = Avboka bokningen

## Booking errors (routes/booking.rs)

booking-error-slot-ended = Det går inte att boka en tid som redan har passerat.
booking-error-description-too-long = Beskrivningen är för lång (max { $max } tecken).
booking-error-maintenance = { $telescope } är på underhåll just nu.
booking-error-limit = Du har nått gränsen på { $max } kommande bokningar.
booking-error-already-booked = Tiden { $time } den { $date } är redan bokad.

## Date format patterns (chrono strftime). Translated so word order can
## differ per language; rendered with the language's chrono locale.

fmt-week-short = %d %b
fmt-week-full = %d %b %Y
fmt-day-col = %a %d

## Observe page

observe-maintenance-mode = Det här teleskopet är i underhållsläge just nu.
observe-wind-warning = Varning för hård vind: vindhastigheten är över säkerhetsgränsen. Parkera teleskopet och återkom när stormen är över. Du kan fortfarande observera, men gör det på egen risk.
observe-guest-session = Gästsession
observe-guest-ends-in = avslutas om
observe-guest-tail = eller tidigare om en registrerad användare bokar det här teleskopet. Data sparas inte.
observe-target = Mål
observe-coord-galactic = Galaktiska
observe-coord-equatorial = Ekvatoriella (J2000)
observe-coord-horizontal = Horisontella
observe-coord-sun = Solen
observe-coord-gnss = GNSS
observe-coord-stow = Parkera
observe-lbl-long = Long. [grader]
observe-lbl-lat = Lat. [grader]
observe-lbl-ra = RA [grader]
observe-lbl-dec = Dekl. [grader]
observe-lbl-az = Az. [grader]
observe-lbl-el = El. [grader]
observe-satellite = Satellit
observe-loading = Laddar...
observe-track = Följ
observe-stop = Stoppa
observe-adv-tracking = Avancerade följningsinställningar
observe-az-offset = Az.-offset [grader]
observe-el-offset = El.-offset [grader]
observe-min-elevation = Lägsta elevation:
observe-recommended = rekommenderat
observe-begin = Starta
observe-end = Avsluta
observe-adv-receiver = Avancerade mottagarinställningar
observe-integration-time = Integrationstid
observe-interactive-end = Interaktivt slut
observe-fixed = Fast:
observe-seconds = sekunder
observe-mode = Läge
observe-freq-switched = Frekvensväxlad
observe-raw = Rå
observe-center-freq = Centerfrekvens [MHz]
observe-ref-freq = Referensfrekvens [MHz]
observe-freq-range = Frekvensområde
observe-bandwidth = Bandbredd
observe-spectral-channels = Spektralkanaler
observe-rfi-filter = RFI-filter (glidande MAD-σ)
observe-enabled = Aktiverat
observe-disabled = Avaktiverat
observe-live-spectrum = Spektrum i realtid
observe-live-webcam = Webbkamera i realtid
observe-webcam-alt = Webbkamerabild i realtid av SALSA-teleskopet
observe-webcam-note = Uppdateras varje sekund. Ingen belysning — teleskopen syns inte efter mörkrets inbrott men kan fortfarande styras.

## Observe page — strings emitted inside JS string literals. Keep them
## free of quotes and apostrophes.

observe-js-time-left = Tid kvar:
observe-js-booking-ending = Bokningen slutar snart – parkera teleskopet eller förläng bokningen.
observe-js-khz-channel = kHz/kanal
observe-js-hz-channel = Hz/kanal
observe-js-integrating = Integrerar:
observe-js-integration-time = Integrationstid:
observe-js-avg-power = Medeleffekt:
observe-js-stopped-1 = Integrationen stoppades: teleskopet lämnade målet. Data som samlats in hittills har sparats i ditt
observe-js-archive = observationsarkiv
observe-js-shortly = inom kort
observe-js-unless = om du inte fortsätter använda kontrollerna nedan
observe-js-max-session = (maximal sessionslängd uppnådd)

## Observe errors (routes/observe.rs)

observe-error-select-satellite = Välj en satellit.
observe-error-invalid-coords = Ange giltiga koordinater.
observe-error-elevation-range = Målet är utanför elevationsområdet ({ $min }–{ $max }°).
observe-error-not-tracking = Teleskopet följer inte målet. Vänta tills det har nått målet.
observe-error-receiver-unreachable = Mottagaren kan inte nås. Kontrollera mottagarens adress och nätverksanslutning.
observe-error-center-freq = Centerfrekvensen måste vara mellan { $min } och { $max } MHz.
observe-error-ref-freq = Referensfrekvensen måste vara mellan { $min } och { $max } MHz.
observe-error-gain = Förstärkningen måste vara mellan { $min } och { $max } dB.

## Telescope status fragment

state-telescope = Teleskop
state-idle = Vilande
state-slewing = Rör sig
state-tracking = Följer
state-offline = Offline
state-offline-error = Kan inte ansluta till teleskopets styrenhet.
state-low-elevation-1 = Låg elevation: teleskopet pekar bara
state-low-elevation-2 = över horisonten — brus från marken och omgivande byggnader kan försämra spektrumet.
state-error-elevation-range = målet är utanför elevationsområdet ({ $min }–{ $max }°)
state-error-io = IO-fel i kommunikationen med teleskopet
state-error-not-connected = teleskopet är inte anslutet
state-error-receiver = mottagaren misslyckades: { $msg }

## Observe landing / no-booking / maintenance pages

observe-landing-none = Inga aktiva bokningar just nu.
observe-landing-go-1 = Gå till sidan
observe-landing-bookings = Bokningar
observe-landing-go-2 = för att boka tid på ett teleskop och återkom hit när din tid är aktiv.
observe-landing-active = Du har aktiva bokningar. Välj ett alternativ nedan:
observe-landing-with = Observera med
observe-landing-point = Rikta teleskopet, välj ett mål och spela in ett spektrum.
observe-landing-interferometry = Interferometri
observe-landing-inter-desc = Använd två teleskop samtidigt för att mäta visibilitet — amplitud och fas — som funktion av baslinje och frekvens.
observe-nobooking-text = Teleskopstyrning är inte tillgänglig utan en giltig bokning.
observe-nobooking-link = Gå till bokningar
observe-maint-heading-1 = Tyvärr är teleskopet
observe-maint-heading-2 = i underhållsläge just nu.
observe-maint-text-1 = Det är inte tillgängligt för observationer. Se
observe-maint-bookings = bokningar
observe-maint-text-2 = för att se om ett annat teleskop är ledigt. Behöver du hjälp, kontakta
observe-maint-support = supporten

## Observations archive page. The spectrum-chart controls (Pick ranges,
## Show frequency, Log scale, the chart hint) are rewritten at runtime by
## assets/observation_chart.js and stay English until that is translated.

obs-tab-single = Enkelteleskop
obs-inter-sessions = Interferometrisessioner
obs-inter-none = Inga interferometrisessioner ännu.
obs-session-singular = session
obs-sessions-plural = sessioner
obs-ended = avslutad
obs-no-end-time = ingen sluttid
obs-inter-delete-confirm = Radera den här interferometrisessionen och all dess visibilitetsdata?
obs-my = Mina observationer
obs-none = Inga observationer ännu.
obs-list-hint = observationer · klicka för att visa spektrum · ✕ för att radera vald
obs-delete-confirm = Radera den här observationen?
obs-spectrum = Spektrum
obs-save-png = Spara PNG
obs-save-csv = Spara CSV
obs-save-fits = Spara FITS
obs-export-note = PNG och CSV exporterar det korrigerade spektrumet · FITS exporterar alltid rådata
obs-analysis = Analys
obs-reset-all = Återställ allt
obs-analysis-note = Analysen gäller bara den här sessionen och sparas inte — anteckna viktiga resultat.
obs-baseline = Baslinje
obs-order = Ordning:
obs-order-linear = 1 (linjär)
obs-order-quadratic = 2 (kvadratisk)
obs-fit = Anpassa
obs-subtract = Subtrahera
obs-clear = Rensa
obs-gaussian = Gaussanpassning

## Live status page and fragments

live-webcam-note = Webbkameran uppdateras varannan sekund; tidsstämpeln är svensk sommartid (CEST = UTC+2). Ingen belysning, så teleskopen syns inte efter mörkrets inbrott men kan fortfarande styras.
live-webcam-alt = Webbkamerabild i realtid av SALSA-teleskopen (Torre, Vale, Brage)
live-telescope-status = Teleskopstatus
live-guest = Gäst
live-position-unavailable = Position saknas
live-controller = Styrenhet
live-receiver = Mottagare
webcam-disabled = Webbkamera avstängd
webcam-offline = Webbkameran är offline — senaste bilden { $age }
webcam-updated = Uppdaterad { $age }
webcam-unavailable = Webbkameran är inte tillgänglig — ingen bild från kameran. Kontakta supporten om problemet kvarstår.
age-secs = { $n } s sedan
age-mins = { $n } min sedan

## Weather fragment. The -js- variants are filled in by page JS with %n%.

weather-heading = Väder i Onsala
weather-temp = Temp
weather-pressure = Tryck
weather-humidity = Fukt
weather-wind = Vind
weather-10min-avg = (10 min medel)
weather-gust = Byvind
weather-lull = lägst
weather-gust-suffix = m/s (3 s max/min under 10 min)
weather-js-age-secs = %n% s sedan
weather-js-age-mins = %n% min sedan

## Target visibility page

vis-back = Support och dokumentation
vis-heading = Målsynlighet
vis-intro-1 = Se när ett givet mål är över horisonten vid SALSA (Onsala) ett valt datum och därmed går att observera. Diagrammet visar elevationen under dygnet, med tider i
vis-intro-2 = rekommenderad undre elevationsgräns som sätts av marken och omgivande byggnader.
vis-coord-system = Koordinatsystem
vis-coord-equatorial = Ekvatoriellt
vis-date = Datum
vis-compute = Beräkna
vis-hover-hint = Håll muspekaren över eller tryck på diagrammet för att läsa av tid och elevation.
vis-lbl-glon = Galaktisk longitud (°)
vis-lbl-glat = Galaktisk latitud (°)
vis-lbl-ra = Rektascension (°)
vis-lbl-dec = Deklination (°)
vis-error-date = Ogiltigt datum — använd ÅÅÅÅ-MM-DD.
vis-error-coord = Ogiltigt koordinatsystem.
vis-target-galactic = Galaktiskt { $x }°, { $y }°
vis-target-equatorial = Ekvatoriellt { $x }°, { $y }°
vis-title = { $target } den { $date } ({ $tz })
vis-not-above = Aldrig över { $threshold }° under dygnet. Högst { $max }° kl. { $peak }.
vis-above = Över { $threshold }° { $windows }. Högst { $max }° kl. { $peak }.
vis-window-range = { $from } till { $to }
vis-window-join = { " " }och{ " " }
vis-axis-utc = UTC-tid
vis-axis-local = Lokal tid ({ $tz })

## Shared chart scripts (assets/observation_chart.js, observe_chart.js).
## Injected as window.CHART_I18N by the layout; the JS falls back to
## English if a key is missing. Keep values free of quotes.

chart-frequency = Frekvens (MHz)
chart-vlsr = VLSR (km/s)
chart-amplitude = Amplitud
chart-linear-scale = Linjär skala
chart-log-scale = Logaritmisk skala
chart-show-frequency = Visa frekvens
chart-show-vlsr = Visa VLSR
chart-done-picking = Klar
chart-pick-ranges = Välj områden
chart-pick-peaks = Välj toppar
chart-hint-range-end = Klicka för att sätta slutet på området
chart-hint-baseline = Klicka för att börja ett baslinjeområde · klicka på Klar när du är färdig
chart-hint-gaussian = Klicka på topparnas mitt för att lägga till startgissningar · klicka på Klar när du är färdig
chart-hint-default = Håll pekaren över för koordinater · rita en ruta för att zooma · dubbelklicka för att återställa
chart-error-not-enough-points = För få datapunkter i de valda områdena för den här polynomordningen.
chart-error-baseline-failed = Baslinjeanpassningen misslyckades (singulär matris).
chart-error-pick-seed = Välj minst en topp som startgissning först.
chart-error-gaussian-failed = Gaussanpassningen misslyckades:
chart-range-singular = område
chart-range-plural = områden
chart-seed-singular = startgissning
chart-seed-plural = startgissningar
chart-picking = (väljer…)
chart-sun-azel = Solen az/el

## Login page

login-same-method-hint = Använd alltid samma inloggningsmetod för att behålla åtkomsten till dina bokningar och observationer.
login-local-summary = Lokal inloggning (endast undantagsfall)
login-local-help = Lokala konton är reserverade för demonstrationer och specialfall. Kontakta SALSA-supporten för att begära åtkomst.
login-invalid-credentials = Fel användarnamn eller lösenord.
login-rate-limited = För många misslyckade försök. Försök igen senare.
login-username = Användarnamn
login-password = Lösenord
login-submit = Logga in
