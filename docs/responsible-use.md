# Responsible Use

Company Feed Server is intended for public company newsrooms, blogs, press
pages, engineering publications, and their RSS or Atom feeds.

Operators should:

- fetch only public pages they are authorized to access;
- respect applicable terms, robots directives, and publisher requests;
- identify their deployment with `PUBLIC_FETCH_USER_AGENT`, including a
  monitored contact address or URL;
- use conservative global and per-host concurrency;
- retain only the content required for the intended product;
- keep private-network access disabled unless testing controlled local
  fixtures;
- review company ownership before public redistribution; and
- put operator APIs behind authentication and network controls.

Do not use this project to bypass authentication, paywalls, access controls,
CAPTCHAs, or technical restrictions. Do not import logged-in browser state or
target personal data.

An accessible URL is evidence that a resource can be fetched, not proof that
it may be republished. See [Data and content policy](data-and-content-policy.md).
