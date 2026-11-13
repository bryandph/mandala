{
  outputs = {self}: {
    metadata = builtins.toJSON ((builtins.removeAttrs self.sourceInfo ["outPath"])
      // {
        originalPath = toString self.sourceInfo.outPath;
        flakePath = toString self.outPath;
        revision = self.rev or self.dirtyRev or "unavailable";
        modified = self.lastModified or 0;
      });
  };
}
