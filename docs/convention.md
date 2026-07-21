# The consumer convention

What a flake must expose for mandala to project it. This is the whole
buy-in surface — the engine reads these and nothing else.

## 1. NixOS members author `host.*` in-config

Every NixOS configuration that should be a fleet member imports the
member schema and authors its facts inside its own evaluation:

```nix
flake.nixosConfigurations.web = nixpkgs.lib.nixosSystem {
  system = "x86_64-linux";
  modules = [
    mandala.lib.schemas.member
    { host = { name = "web"; domain = "fleet.example"; role = "web";
               deployment.ansible.enable = true;
               deployment.deployRs.enable = true; }; }
    # ... the actual configuration
  ];
};
```

The validated `config.host` IS the member: there is no registry to
update and nothing to keep in sync — the arrow points outward, from
configurations to projections. Management surfaces (`deployment.ansible`,
`deployment.deployRs`) default OFF; a fleet's configuration factory
typically `mkDefault`s them on for the hosts it builds.

## 2. Non-NixOS members are validated plain data

Routers, switches, BMCs, phones — anything with a network identity but
no NixOS evaluation — are member data validated through
`lib.evalMember` and handed to the fleet module:

```nix
mandala.extraMembers.router = mandala.lib.evalMember {
  name = "router";
  platform = "opnsense";
  category = "gateway";
};
```

With no management surface enabled they are facts-only: present in the
taxonomy and data projections, untouched by deploy/ansible.

## 3. Import the flakeModules you want

```nix
imports = [
  mandala.flakeModules.fleet    # flake.mandala — the versioned aggregate
  mandala.flakeModules.ansible  # flake.ansibleInventory
  mandala.flakeModules.sops     # flake.sopsConfig (needs mandala.sops.*)
  mandala.flakeModules.deploy   # flake.deploy.nodes + deployBatch
];
```

Each tool module imports `fleet` for you. They contain wiring + hook
options only; the same projections are callable directly from
`mandala.lib.projections.*` for non-flake-parts consumers.

## 4. Toolchains are YOUR inputs

The engine pins nixpkgs and nothing else. The deploy module resolves
`inputs.deploy-rs` and `inputs.nixpkgs` from the CONSUMING flake by
convention — declare them yourself (you own the versions). The same
applies to terranix, sops, ansible: mandala projects data for them, it
never carries them.

## 5. The aggregate is the porcelain contract

`nix eval --json .#mandala` returns
`{schemaVersion, members, groups, projections, operator?}` — pure data,
one eval, gated by `schemaVersion`. The CLI and plugged engines read
ONLY this; per-tool outputs (`.#ansibleInventory`, `.#sopsConfig`,
`deploy.nodes`, `.#deployBatch.<group>`) remain for direct tool
consumption. Groups use the ansible-safe spelling everywhere, so
`@k3s`, `ansible -l k3s`, and `.#deployBatch.k3s` are one name for one
member set.

## 6. Deploy settings are flattened once

Deploy settings have three authoring tiers:

- fleet defaults at `mandala.deployment.settings`;
- group settings at `mandala.deployment.groupSettings.<group>`; and
- member settings at `host.deployment.deployRs`, with the SSH target,
  login, and port under `host.deployment.ssh`.

For scalar values, the member tier wins over the group tier, which wins
over the fleet tier. A member can belong to several configured groups;
those group names are sorted lexicographically and folded in that order,
so the later group wins a scalar conflict. `sshOpts` is additive instead:
member options come first, followed by applicable groups in that same
sorted order, then fleet options.

`groupSettings` keys must use the sanitized canonical taxonomy spelling
exposed by `flake.mandala.groups`. An unknown key, including an unsanitized
spelling that does not exist there, fails evaluation. Member
`autoRollback` and `fastConnection` values are nullable authoring choices;
after all tiers merge, each still defaults effectively to `true` when no
tier supplied it, preserving the historical deploy-node behavior.

The resolved inspection surface is
`flake.mandala.projections.deploy.settings.<member>`. Both the native
engine and `flake.deploy.nodes` consume that flattened data; consumers
must not repeat the merge. See `schema/member.nix`,
`flake-modules/fleet.nix`, and `lib/deploy-settings.nix` for the source of
truth, and `examples/showcase/` for an evaluated member/group example.

## 7. Hooks, not forks

Repo-specific values enter through hook options, never by patching the
engine: `mandala.ansible.extraHostvars` (per-member vars, merged after —
and able to override — the engine defaults), `mandala.ansible.{pythonInterpreter,guardGroup}`
(NixOS conventions, on by default), `mandala.sops.declarations`
(secret-grade declarations; see `schema/secrets.nix`).

Start from the template (`nix flake init -t github:bryandph/mandala#fleet`),
then read `examples/showcase/` for every projection in one place and
`docs/patterns/` for the terranix and imperative-ansible shapes.
