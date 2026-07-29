# Security policy

## Supported version

Only the current `main` publication contract and validation workflow are
maintained. Historical article snapshots are data records, not supported
software releases.

## Reporting

Do not publish exploit details, credentials, access tokens, personal data, or
other sensitive information in an issue. Use the repository's
[private vulnerability reporting form](https://github.com/Shuozeli/company-news-data/security/advisories/new)
for a vulnerability in the archive format, validator, workflow, or static-read
contract.

For a non-sensitive security-hardening suggestion, open a
[repository issue](https://github.com/Shuozeli/company-news-data/issues/new)
and describe the affected schema, validator, workflow, or read contract.

Data accuracy and extraction problems are not security vulnerabilities. Use the
[data-correction form](https://github.com/Shuozeli/company-news-data/issues/new?template=data-correction.yml).
Copyright, permission, attribution, and removal concerns belong in the
[rights and removal form](https://github.com/Shuozeli/company-news-data/issues/new?template=rights.yml).

## Scope

Useful reports include:

- unsafe path resolution or hash-validation bypasses;
- workflow permissions or secret-exposure problems;
- malformed archive content that can exploit a conforming consumer; and
- contract behavior that causes clients to fetch outside the declared static
  archive.

The canonical publisher pages are third-party systems and are outside this
repository's security scope.
