# Decisions

Resolve the CA principal after route-over-member connection precedence. Use the
route hostname, member builder hostname, or member FQDN, whichever supplies the
effective dialled endpoint. Never use a generated alias, port-qualified name,
jump endpoint, or an inferred certificate name. Explicit principals win even
when different from the dialled hostname. Resolve jump-only routes identically.

Facet normalization permits an unresolved CA principal because one facet can
serve controllers dialing different names. Projection requires a valid endpoint
and a non-null effective principal, failing closed when no name is derivable.
Pins still reject principals, retain member-specific aliases and encode the full
public key. The existing renderer needs no change: it consumes resolved trust.

Alternatives: defaulting at facet evaluation would miss per-controller routes;
requiring explicit principals preserves duplication; inferring certificate
contents would violate pure evaluation. The endpoint default is preferred.

Non-VM external builders replace the earlier hosted-guest examples. This is an
example recast, not a new host-type implementation. Legacy byte-parity goldens
remain immutable. Existing adapter goldens change only for the renamed builder
and its endpoint; a separate new golden covers a defaulted short-name CA.
