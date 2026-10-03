// Headless Ghidra post-script: locates strings and imports by name, lists everything
// that references them and decompiles the referencing functions. For a string that is
// referenced from data (a registration table of {name, function} pairs, as the Enforce
// native bindings are), the pointers around the referencing slot that land in
// executable memory are reported and decompiled as well.
//
//   -postScript FindRefs.java <out dir> <query> [<query> ...]
//   query: string:<exact text> | import:<symbol name> | addr:<rva hex> (references to that
//          address, e.g. an IAT slot or a global) | table:<rva hex>:<count> (a pointer
//          table such as a vtable: decompiles every slot that points into code) |
//          vtables:<text> (every RTTI class whose name contains the text: lists its vftable
//          address and the code slots, without decompiling)
//
// Output: <out dir>/refs-<query>.txt (the report) and DayZ+0x<entry>.c per function.
// Driven by scripts/ghidra-decompile.sh.
//@category DayZ-VR

import java.io.File;
import java.io.FileWriter;
import java.io.PrintWriter;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.data.StringDataInstance;
import ghidra.program.model.listing.Data;
import ghidra.program.model.listing.DataIterator;
import ghidra.program.model.listing.Function;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.Symbol;
import ghidra.program.model.symbol.SymbolIterator;

public class FindRefs extends GhidraScript {
    private Address base;
    private DecompInterface decompiler;
    private File outDir;
    private final Set<Long> decompiled = new HashSet<>();

    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        if (args.length < 2) {
            printerr("usage: FindRefs.java <out dir> <string:text|import:name> ...");
            return;
        }
        outDir = new File(args[0]);
        if (!outDir.isDirectory() && !outDir.mkdirs()) {
            printerr("cannot create " + outDir);
            return;
        }
        base = currentProgram.getImageBase();
        decompiler = new DecompInterface();
        decompiler.toggleCCode(true);
        decompiler.toggleSyntaxTree(false);
        decompiler.setSimplificationStyle("decompile");
        if (!decompiler.openProgram(currentProgram)) {
            printerr("decompiler failed to open the program: " + decompiler.getLastMessage());
            return;
        }
        try {
            for (int index = 1; index < args.length; index++) {
                String query = args[index];
                File report = new File(outDir, "refs-" + query.replaceAll("[^A-Za-z0-9_.-]", "_") + ".txt");
                try (PrintWriter writer = new PrintWriter(new FileWriter(report))) {
                    if (query.startsWith("string:")) {
                        findString(query.substring(7), writer);
                    } else if (query.startsWith("import:")) {
                        findImport(query.substring(7), writer);
                    } else if (query.startsWith("vtables:")) {
                        listVtables(query.substring(8), writer);
                    } else if (query.startsWith("table:")) {
                        String[] parts = query.substring(6).split(":");
                        long rva = Long.parseLong(parts[0].replaceFirst("^0[xX]", ""), 16);
                        int count = parts.length > 1 ? Integer.parseInt(parts[1]) : 32;
                        dumpTable(base.add(rva), count, writer);
                    } else if (query.startsWith("addr:")) {
                        long rva = Long.parseLong(query.substring(5).replaceFirst("^0[xX]", ""), 16);
                        writer.printf("address DayZ+0x%X%n", rva);
                        reportReferences(base.add(rva), writer, false);
                    } else {
                        writer.println("unknown query " + query);
                    }
                }
                println("wrote " + report);
            }
        } finally {
            decompiler.dispose();
        }
    }

    private void findString(String text, PrintWriter writer) throws Exception {
        writer.println("string \"" + text + "\"");
        DataIterator data = currentProgram.getListing().getDefinedData(true);
        int hits = 0;
        while (data.hasNext() && !monitor.isCancelled()) {
            Data item = data.next();
            if (!item.hasStringValue()) {
                continue;
            }
            String value = StringDataInstance.getStringDataInstance(item).getStringValue();
            if (value == null || !value.equals(text)) {
                continue;
            }
            hits++;
            writer.printf("  defined at DayZ+0x%X%n", item.getAddress().subtract(base));
            reportReferences(item.getAddress(), writer, true);
        }
        if (hits == 0) {
            // Not a defined string: search the raw bytes (UTF-8, NUL terminated).
            Address found = currentProgram.getMinAddress();
            while ((found = currentProgram.getMemory().findBytes(found, (text + "\0").getBytes("UTF-8"),
                null, true, monitor)) != null) {
                hits++;
                writer.printf("  raw bytes at DayZ+0x%X%n", found.subtract(base));
                reportReferences(found, writer, true);
                found = found.add(1);
            }
        }
        if (hits == 0) {
            writer.println("  not found");
        }
    }

    private void findImport(String name, PrintWriter writer) throws Exception {
        writer.println("import " + name);
        SymbolIterator symbols = currentProgram.getSymbolTable().getSymbols(name);
        boolean any = false;
        while (symbols.hasNext()) {
            Symbol symbol = symbols.next();
            any = true;
            writer.printf("  symbol %s at %s (%s)%n", symbol.getName(true), symbol.getAddress(),
                symbol.getSymbolType());
            reportReferences(symbol.getAddress(), writer, false);
        }
        if (!any) {
            writer.println("  not found");
        }
    }

    // Lists the references to an address; code references get their function decompiled,
    // data references (pointer slots) get the neighbouring code pointers reported as well.
    // Thunks (an import's IAT slot is reached through a jmp stub) are followed one level.
    private void reportReferences(Address target, PrintWriter writer, boolean tableNeighbours) throws Exception {
        List<Reference> refs = new ArrayList<>();
        for (Reference ref : currentProgram.getReferenceManager().getReferencesTo(target)) {
            refs.add(ref);
        }
        writer.printf("  %d reference(s)%n", refs.size());
        for (Reference ref : refs) {
            Address from = ref.getFromAddress();
            Function function = getFunctionContaining(from);
            writer.printf("    from DayZ+0x%X %s in %s%n", from.subtract(base), ref.getReferenceType(),
                function == null ? "(data)" : function.getName() + " DayZ+0x"
                    + Long.toHexString(function.getEntryPoint().subtract(base)).toUpperCase());
            if (function != null) {
                if (function.isThunk() || function.getBody().getNumAddresses() <= 16) {
                    writer.println("      stub; callers:");
                    for (Reference caller : currentProgram.getReferenceManager()
                            .getReferencesTo(function.getEntryPoint())) {
                        Function callerFunction = getFunctionContaining(caller.getFromAddress());
                        writer.printf("        DayZ+0x%X in %s%n", caller.getFromAddress().subtract(base),
                            callerFunction == null ? "?" : callerFunction.getName() + " DayZ+0x"
                                + Long.toHexString(callerFunction.getEntryPoint().subtract(base)).toUpperCase());
                        decompile(callerFunction, writer);
                    }
                } else {
                    decompile(function, writer);
                }
            } else if (tableNeighbours) {
                for (int slot = -4; slot <= 4; slot++) {
                    if (slot == 0) {
                        continue;
                    }
                    Address slotAddress = from.add(slot * 8L);
                    long value;
                    try {
                        value = currentProgram.getMemory().getLong(slotAddress);
                    } catch (Exception e) {
                        continue;
                    }
                    Address pointer = base.getNewAddress(value);
                    MemoryBlock block = currentProgram.getMemory().getBlock(pointer);
                    if (block == null || !block.isExecute()) {
                        continue;
                    }
                    Function candidate = getFunctionContaining(pointer);
                    if (candidate == null) {
                        candidate = createFunction(pointer, null);
                    }
                    writer.printf("      slot %+d -> code DayZ+0x%X%s%n", slot, pointer.subtract(base),
                        candidate == null ? "" : " (" + candidate.getName() + ")");
                    decompile(candidate, writer);
                }
            }
        }
    }

    // Lists every "vftable" symbol whose namespace contains the text, with its slots.
    private void listVtables(String text, PrintWriter writer) throws Exception {
        SymbolIterator symbols = currentProgram.getSymbolTable().getSymbols("vftable");
        int classes = 0;
        while (symbols.hasNext() && !monitor.isCancelled()) {
            Symbol symbol = symbols.next();
            String qualified = symbol.getName(true);
            if (!qualified.contains(text)) {
                continue;
            }
            classes++;
            writer.printf("%s at DayZ+0x%X%n", qualified, symbol.getAddress().subtract(base));
            for (int slot = 0; slot < 96; slot++) {
                long value;
                try {
                    value = currentProgram.getMemory().getLong(symbol.getAddress().add(slot * 8L));
                } catch (Exception e) {
                    break;
                }
                Address pointer = base.getNewAddress(value);
                MemoryBlock block = currentProgram.getMemory().getBlock(pointer);
                if (block == null || !block.isExecute()) {
                    break;
                }
                Function function = getFunctionContaining(pointer);
                writer.printf("  slot %2d (+0x%03X): DayZ+0x%X%s%n", slot, slot * 8, pointer.subtract(base),
                    function == null ? "" : " " + function.getName() + " " + function.getBody().getNumAddresses() + " bytes");
            }
        }
        writer.printf("%d class(es)%n", classes);
    }

    // Reports the code pointers of a table (vtable, dispatch table) and decompiles them.
    private void dumpTable(Address table, int count, PrintWriter writer) throws Exception {
        writer.printf("table DayZ+0x%X, %d slots%n", table.subtract(base), count);
        for (int slot = 0; slot < count; slot++) {
            long value;
            try {
                value = currentProgram.getMemory().getLong(table.add(slot * 8L));
            } catch (Exception e) {
                break;
            }
            Address pointer = base.getNewAddress(value);
            MemoryBlock block = currentProgram.getMemory().getBlock(pointer);
            if (block == null || !block.isExecute()) {
                writer.printf("  slot %2d (+0x%03X): not code (0x%X)%n", slot, slot * 8, value);
                continue;
            }
            Function function = getFunctionContaining(pointer);
            if (function == null) {
                function = createFunction(pointer, null);
            }
            writer.printf("  slot %2d (+0x%03X): DayZ+0x%X%s%n", slot, slot * 8, pointer.subtract(base),
                function == null ? "" : " " + function.getName() + " " + function.getBody().getNumAddresses() + " bytes");
            decompile(function, writer);
        }
    }

    private void decompile(Function function, PrintWriter writer) throws Exception {
        if (function == null) {
            return;
        }
        long entryRva = function.getEntryPoint().subtract(base);
        if (!decompiled.add(entryRva)) {
            return;
        }
        DecompileResults results = decompiler.decompileFunction(function, 120, monitor);
        File out = new File(outDir, String.format("DayZ+0x%X.c", entryRva));
        try (PrintWriter code = new PrintWriter(new FileWriter(out))) {
            code.printf("// %s at DayZ+0x%X, %d bytes%n", function.getName(), entryRva,
                function.getBody().getNumAddresses());
            if (results == null || !results.decompileCompleted()) {
                code.println("// decompilation failed: "
                    + (results == null ? "null" : results.getErrorMessage()));
            } else {
                code.println(results.getDecompiledFunction().getC());
            }
        }
        writer.printf("      decompiled -> %s%n", out.getName());
    }
}
