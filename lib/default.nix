# Engine library. Pure nixpkgs.lib — no packages, no operator data.
{lib}: rec {
  # Schema modules (paths, importable into any module evaluation).
  schemas = {
    operator = ../schema/operator.nix;
  };

  # Evaluate operator data against the schema; returns the validated
  # operator attrset including derived fields. Invalid data fails the
  # consumer's eval, not a later deploy.
  evalOperator = data:
    (lib.evalModules {
      modules = [
        schemas.operator
        {operator = data;}
      ];
    }).config.operator;
}
