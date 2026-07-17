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

## Login page

login-same-method-hint = Använd alltid samma inloggningsmetod för att behålla åtkomsten till dina bokningar och observationer.
login-local-summary = Lokal inloggning (endast undantagsfall)
login-local-help = Lokala konton är reserverade för demonstrationer och specialfall. Kontakta SALSA-supporten för att begära åtkomst.
login-invalid-credentials = Fel användarnamn eller lösenord.
login-rate-limited = För många misslyckade försök. Försök igen senare.
login-username = Användarnamn
login-password = Lösenord
login-submit = Logga in
