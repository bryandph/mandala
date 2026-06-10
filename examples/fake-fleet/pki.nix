# Fake PKI used by `checks.<system>.fake-fleet`. Not real certificates.
{
  cas = {
    example-root = {
      kind = "root";
      description = "Example Root CA";
      pem = ''
        -----BEGIN CERTIFICATE-----
        ZmFrZS1yb290LWNhLW5vdC1hLXJlYWwtY2VydGlmaWNhdGU=
        -----END CERTIFICATE-----
      '';
    };
    example-intermediate = {
      kind = "intermediate";
      signedBy = "example-root";
      description = "Example Intermediate CA";
      pem = ''
        -----BEGIN CERTIFICATE-----
        ZmFrZS1pbnRlcm1lZGlhdGUtY2EtYWxzby1ub3QtcmVhbA==
        -----END CERTIFICATE-----
      '';
    };
  };
}
