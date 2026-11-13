{
  inputs.child.url = "path:./child";
  outputs = {
    self,
    child,
  }: let
    system = builtins.derivation {
      name = "campaign-metadata-fixture";
      system = "x86_64-linux";
      # This fixture must never be deployed or built successfully.
      builder = "/bin/false";
      metadata = builtins.toJSON ((builtins.removeAttrs self.sourceInfo ["outPath"])
        // {
          originalPath = toString self.sourceInfo.outPath;
          flakePath = toString self.outPath;
          revision = self.rev or self.dirtyRev or "unavailable";
          modified = self.lastModified or 0;
        });
      childMetadata = child.metadata;
    };
  in {
    mandala = {
      schemaVersion = 1;
      members.node-a = {
        name = "node-a";
        deployment.deployRs.enable = true;
      };
      groups.fleet = ["node-a"];
      projections.deploy = {
        nodes = ["node-a"];
        settings.node-a = {
          activation = "switch";
          hostname = "node-a.invalid";
          sshUser = "deployer";
          sshPort = 22;
          identityFile = null;
          autoRollback = true;
          fastConnection = true;
        };
      };
    };
    nixosConfigurations.node-a.config.system.build.toplevel = system;
    deploy.nodes.node-a.profiles.system.path = system;
  };
}
