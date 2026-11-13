# Replay the captured node contract used by Nix 2.34.x call-flake.nix.
# Fetch only fixed-NAR archived paths; original fetcher metadata is data.
graph: let
  nodes =
    builtins.mapAttrs (_: node: let
      sourceInfo =
        if node.relative != null
        then
          if node.relative_override == null
          then nodes.${node.parent}.sourceInfo
          else node.relative_override // {outPath = nodes.${node.parent}.sourceInfo.outPath;}
        else let
          tree = builtins.fetchTree {
            type = "path";
            path = node.source.path;
            narHash = node.source.metadata.narHash;
          };
        in
          node.source.metadata // {outPath = tree.outPath;};
      outPath =
        if node.relative != null
        then
          if node.relative_override == null
          then
            nodes.${node.parent}.outPath
            + (
              if node.relative == ""
              then ""
              else "/" + node.relative
            )
          else
            sourceInfo.outPath
            + (
              if node.relative_dir == ""
              then ""
              else "/" + node.relative_dir
            )
        else
          sourceInfo.outPath
          + (
            if node.dir == ""
            then ""
            else "/" + node.dir
          );
      inputs = builtins.mapAttrs (_: target: nodes.${target}.result) node.inputs;
      authored = import (outPath + "/flake.nix");
      outputs = authored.outputs (inputs // {self = result;});
      result =
        if node.flake
        then
          outputs
          // sourceInfo
          // {
            inherit outPath inputs outputs sourceInfo;
            _type = "flake";
          }
        else sourceInfo // {inherit outPath sourceInfo;};
    in {
      inherit result sourceInfo outPath;
    })
    graph.nodes;
in
  nodes.${graph.root}.result.outputs
