{lib}: let
  fixture = import ./fixtures/vm-owner-artifacts.nix {inherit lib;};
  manifest = fixture.manifest;
  context = builtins.getContext (builtins.toJSON manifest);
  ownerPath = fixture.owner.config.system.build.toplevel.drvPath;
  command = fixture.owner.config.system.systemBuilderCommands;
in
  assert fixture.compatible manifest;
  assert !(fixture.compatible fixture.unrelated);
  assert lib.all (name: context ? ${builtins.unsafeDiscardStringContext fixture.guests.${name}.config.system.build.toplevel.drvPath}) ["guest-a" "guest-b"];
  assert !(context ? ${builtins.unsafeDiscardStringContext ownerPath});
  assert lib.hasInfix "mandala/vm-application.json" command;
  assert lib.hasInfix "@OWNER_GENERATION@" command;
  assert lib.hasPrefix "/nix/store/" ownerPath; true
