// Headless Ghidra post-script: decompiles the functions containing the given RVAs
// (hex, relative to the image base) and writes one C file per function plus the
// list of direct callers into the output directory.
//
//   analyzeHeadless <project dir> <project> -import DayZ_x64.exe \
//       -scriptPath scripts/ghidra -postScript DecompileRvas.java <out dir> <rva> [<rva> ...]
//
// Driven by scripts/ghidra-decompile.sh; see that file for the full invocation.
//@category DayZ-VR

import java.io.File;
import java.io.FileWriter;
import java.io.PrintWriter;
import java.util.HashSet;
import java.util.Set;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.ReferenceIterator;

public class DecompileRvas extends GhidraScript {
    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        if (args.length < 2) {
            printerr("usage: DecompileRvas.java <out dir> <rva> [<rva> ...]");
            return;
        }
        File outDir = new File(args[0]);
        if (!outDir.isDirectory() && !outDir.mkdirs()) {
            printerr("cannot create " + outDir);
            return;
        }
        Address base = currentProgram.getImageBase();
        DecompInterface decompiler = new DecompInterface();
        decompiler.toggleCCode(true);
        decompiler.toggleSyntaxTree(false);
        decompiler.setSimplificationStyle("decompile");
        if (!decompiler.openProgram(currentProgram)) {
            printerr("decompiler failed to open the program: " + decompiler.getLastMessage());
            return;
        }
        Set<Long> done = new HashSet<>();
        try {
            for (int index = 1; index < args.length; index++) {
                long rva = Long.parseLong(args[index].replaceFirst("^0[xX]", ""), 16);
                Address address = base.add(rva);
                Function function = getFunctionContaining(address);
                if (function == null) {
                    function = createFunction(address, null);
                }
                if (function == null) {
                    printerr("no function at RVA 0x" + Long.toHexString(rva));
                    continue;
                }
                long entryRva = function.getEntryPoint().subtract(base);
                if (!done.add(entryRva)) {
                    continue;
                }
                DecompileResults results = decompiler.decompileFunction(function, 120, monitor);
                File out = new File(outDir, String.format("DayZ+0x%X.c", entryRva));
                try (PrintWriter writer = new PrintWriter(new FileWriter(out))) {
                    writer.printf("// %s at DayZ+0x%X (requested 0x%X), %d bytes%n",
                        function.getName(), entryRva, rva, function.getBody().getNumAddresses());
                    writer.println("// callers:");
                    ReferenceIterator refs = currentProgram.getReferenceManager()
                        .getReferencesTo(function.getEntryPoint());
                    while (refs.hasNext()) {
                        Reference ref = refs.next();
                        Function caller = getFunctionContaining(ref.getFromAddress());
                        writer.printf("//   DayZ+0x%X (%s) in %s%n", ref.getFromAddress().subtract(base),
                            ref.getReferenceType(), caller == null ? "?" : caller.getName());
                    }
                    if (results == null || !results.decompileCompleted()) {
                        writer.println("// decompilation failed: "
                            + (results == null ? "null" : results.getErrorMessage()));
                    } else {
                        writer.println(results.getDecompiledFunction().getC());
                    }
                }
                println("wrote " + out);
            }
        } finally {
            decompiler.dispose();
        }
    }
}
