# Fake operator instance used by `checks.<system>.fake-fleet`. No value
# here belongs to a real fleet or person.
{
  name = "operator";
  fullname = "Example Operator";
  email = "operator@example.com";
  gpg = {
    fingerprint = "0123456789ABCDEF0123456789ABCDEF01234567";
    publicKeyUrl = "https://example.com/keys/operator.asc";
  };
  sshPublicKeys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderPlaceholderPlaceholderPlacehol operator@example"
  ];
}
