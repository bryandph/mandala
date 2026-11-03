# Synthetic deploy-rs activation shim; never imports nixpkgs or builds a host.
_: {
  deploy-rs.lib.activate = {
    nixos = _: "/synthetic/activate";
    custom = _: _: "/synthetic/activate";
  };
}
