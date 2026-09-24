{
  lib,
  pkgs,
}: {
  name = "mandala-switch-inhibitor-staging";

  nodes.machine = {lib, ...}: {
    virtualisation = {
      useBootLoader = true;
      useEFIBoot = true;
    };
    boot.loader = {
      systemd-boot.enable = true;
      efi.canTouchEfiVariables = true;
    };

    system.switch.inhibitors.mandala-test = "old";
    environment.etc."mandala-generation".text = "old";

    specialisation.staged.configuration = {
      system.switch.inhibitors.mandala-test = lib.mkForce "new";
      environment.etc."mandala-generation".text = lib.mkForce "staged";
    };
  };

  testScript = {nodes, ...}: let
    originalSystem = nodes.machine.system.build.toplevel;
    stagedSystem = nodes.machine.specialisation.staged.configuration.system.build.toplevel;
  in
    # python
    ''
      system_profile = "/nix/var/nix/profiles/system"

      def resolved(path):
          return machine.succeed(f"readlink -e {path}").strip()

      machine.wait_for_unit("multi-user.target")
      assert resolved("/run/current-system") == "${originalSystem}"
      assert machine.succeed("cat /etc/mandala-generation").strip() == "old"

      # Establish the same recoverable prerequisite Mandala requires before
      # it stages a new boot default.
      machine.succeed(
          "${pkgs.nix}/bin/nix-env "
          f"--profile {system_profile} --set ${originalSystem}"
      )
      machine.succeed("${originalSystem}/bin/switch-to-configuration boot")
      assert resolved(system_profile) == "${originalSystem}"

      with subtest("changed inhibitor refuses live activation"):
          machine.succeed("test -e ${stagedSystem}/switch-inhibitors")
          out = machine.fail(
              "env -u NIXOS_NO_CHECK "
              "${stagedSystem}/bin/switch-to-configuration check 2>&1"
          )
          assert "There are changes to critical components" in out
          assert "mandala-test" in out
          assert resolved("/run/current-system") == "${originalSystem}"
          assert resolved(system_profile) == "${originalSystem}"

      with subtest("boot-only transaction leaves the running system alone"):
          machine.succeed(
              "${pkgs.nix}/bin/nix-env "
              f"--profile {system_profile} --set ${stagedSystem}"
          )
          machine.succeed("${stagedSystem}/bin/switch-to-configuration boot")
          assert resolved("/run/current-system") == "${originalSystem}"
          assert resolved(system_profile) == "${stagedSystem}"
          assert machine.succeed("cat /etc/mandala-generation").strip() == "old"

      with subtest("staged generation becomes current only after reboot"):
          machine.succeed("sync")
          machine.crash()
          machine.wait_for_unit("multi-user.target")
          assert resolved("/run/current-system") == "${stagedSystem}"
          assert resolved(system_profile) == "${stagedSystem}"
          assert machine.succeed("cat /etc/mandala-generation").strip() == "staged"
    '';
}
