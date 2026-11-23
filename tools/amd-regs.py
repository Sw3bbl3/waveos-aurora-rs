#!/usr/bin/env python3
"""Generates libs/amdgfx/src/layout.rs: byte offsets of the display
registers WaveOS uses, per display IP version, from AMD's published
register headers (asic_reg/dcn/*_offset.h, asic_reg/dce/*). Usage:
    amd-regs.py HEADER_DIR > layout.rs
Register address = (segment base[BASE_IDX] + offset) * 4; the DCN/DCE
segment bases (0x12, 0xC0, 0x34C0, 0x9000, 0x02403C00) are the ones in
the ip_offset headers of every ASIC that has one."""
import re
import sys

BASES = [0x12, 0xC0, 0x34C0, 0x9000, 0x02403C00]

DCN = [
    ("otg_control", ["OTG{i}_OTG_CONTROL"]),
    ("otg_h_total", ["OTG{i}_OTG_H_TOTAL"]),
    ("otg_h_blank", ["OTG{i}_OTG_H_BLANK_START_END"]),
    ("otg_h_sync", ["OTG{i}_OTG_H_SYNC_A"]),
    ("otg_h_sync_cntl", ["OTG{i}_OTG_H_SYNC_A_CNTL"]),
    ("otg_v_total", ["OTG{i}_OTG_V_TOTAL"]),
    ("otg_v_blank", ["OTG{i}_OTG_V_BLANK_START_END"]),
    ("otg_v_sync", ["OTG{i}_OTG_V_SYNC_A"]),
    ("otg_v_sync_cntl", ["OTG{i}_OTG_V_SYNC_A_CNTL"]),
    ("otg_interlace", ["OTG{i}_OTG_INTERLACE_CONTROL"]),
    ("otg_frame_count", ["OTG{i}_OTG_STATUS_FRAME_COUNT"]),
    ("hubp_cntl", ["HUBP{i}_DCHUBP_CNTL"]),
    ("surface_config", ["HUBP{i}_DCSURF_SURFACE_CONFIG"]),
    ("tiling_config", ["HUBP{i}_DCSURF_TILING_CONFIG"]),
    ("viewport_start", ["HUBP{i}_DCSURF_PRI_VIEWPORT_START"]),
    ("viewport_dim", ["HUBP{i}_DCSURF_PRI_VIEWPORT_DIMENSION"]),
    ("pitch", ["HUBPREQ{i}_DCSURF_SURFACE_PITCH"]),
    ("addr", ["HUBPREQ{i}_DCSURF_PRIMARY_SURFACE_ADDRESS"]),
    ("addr_high", ["HUBPREQ{i}_DCSURF_PRIMARY_SURFACE_ADDRESS_HIGH"]),
    ("surface_control", ["HUBPREQ{i}_DCSURF_SURFACE_CONTROL"]),
    ("flip_control", ["HUBPREQ{i}_DCSURF_FLIP_CONTROL"]),
    ("cur_settings", ["HUBPREQ{i}_CURSOR_SETTINS", "HUBPREQ{i}_CURSOR_SETTINGS"]),
    ("cur_control", ["CURSOR{i}_CURSOR_CONTROL", "CURSOR0_{i}_CURSOR_CONTROL"]),
    ("cur_addr", ["CURSOR{i}_CURSOR_SURFACE_ADDRESS", "CURSOR0_{i}_CURSOR_SURFACE_ADDRESS"]),
    ("cur_addr_high", ["CURSOR{i}_CURSOR_SURFACE_ADDRESS_HIGH", "CURSOR0_{i}_CURSOR_SURFACE_ADDRESS_HIGH"]),
    ("cur_size", ["CURSOR{i}_CURSOR_SIZE", "CURSOR0_{i}_CURSOR_SIZE"]),
    ("cur_position", ["CURSOR{i}_CURSOR_POSITION", "CURSOR0_{i}_CURSOR_POSITION"]),
    ("cur_hot_spot", ["CURSOR{i}_CURSOR_HOT_SPOT", "CURSOR0_{i}_CURSOR_HOT_SPOT"]),
    ("cur_dst_offset", ["CURSOR{i}_CURSOR_DST_OFFSET", "CURSOR0_{i}_CURSOR_DST_OFFSET"]),
    ("dpp_cur_control", ["CNVC_CUR{i}_CURSOR0_CONTROL"]),
    ("hpd_status", ["HPD{i}_DC_HPD_INT_STATUS"]),
    ("aux_control", ["DP_AUX{i}_AUX_CONTROL"]),
    ("aux_sw_control", ["DP_AUX{i}_AUX_SW_CONTROL"]),
    ("aux_arb_control", ["DP_AUX{i}_AUX_ARB_CONTROL"]),
    ("aux_interrupt_control", ["DP_AUX{i}_AUX_INTERRUPT_CONTROL"]),
    ("aux_sw_status", ["DP_AUX{i}_AUX_SW_STATUS"]),
    ("aux_sw_data", ["DP_AUX{i}_AUX_SW_DATA"]),
]

DCE = [
    ("crtc_control", ["CRTC{i}_CRTC_CONTROL"]),
    ("crtc_h_total", ["CRTC{i}_CRTC_H_TOTAL"]),
    ("crtc_h_blank", ["CRTC{i}_CRTC_H_BLANK_START_END"]),
    ("crtc_h_sync", ["CRTC{i}_CRTC_H_SYNC_A"]),
    ("crtc_h_sync_cntl", ["CRTC{i}_CRTC_H_SYNC_A_CNTL"]),
    ("crtc_v_total", ["CRTC{i}_CRTC_V_TOTAL"]),
    ("crtc_v_blank", ["CRTC{i}_CRTC_V_BLANK_START_END"]),
    ("crtc_v_sync", ["CRTC{i}_CRTC_V_SYNC_A"]),
    ("crtc_v_sync_cntl", ["CRTC{i}_CRTC_V_SYNC_A_CNTL"]),
    ("crtc_interlace", ["CRTC{i}_CRTC_INTERLACE_CONTROL"]),
    ("crtc_frame_count", ["CRTC{i}_CRTC_STATUS_FRAME_COUNT"]),
    ("grph_enable", ["DCP{i}_GRPH_ENABLE"]),
    ("grph_control", ["DCP{i}_GRPH_CONTROL"]),
    ("grph_swap", ["DCP{i}_GRPH_SWAP_CNTL"]),
    ("grph_addr", ["DCP{i}_GRPH_PRIMARY_SURFACE_ADDRESS"]),
    ("grph_addr_high", ["DCP{i}_GRPH_PRIMARY_SURFACE_ADDRESS_HIGH"]),
    ("grph_pitch", ["DCP{i}_GRPH_PITCH"]),
    ("grph_offset_x", ["DCP{i}_GRPH_SURFACE_OFFSET_X"]),
    ("grph_offset_y", ["DCP{i}_GRPH_SURFACE_OFFSET_Y"]),
    ("grph_x_start", ["DCP{i}_GRPH_X_START"]),
    ("grph_y_start", ["DCP{i}_GRPH_Y_START"]),
    ("grph_x_end", ["DCP{i}_GRPH_X_END"]),
    ("grph_y_end", ["DCP{i}_GRPH_Y_END"]),
    ("grph_update", ["DCP{i}_GRPH_UPDATE"]),
    ("grph_flip_control", ["DCP{i}_GRPH_FLIP_CONTROL"]),
    ("cur_control", ["DCP{i}_CUR_CONTROL"]),
    ("cur_addr", ["DCP{i}_CUR_SURFACE_ADDRESS"]),
    ("cur_addr_high", ["DCP{i}_CUR_SURFACE_ADDRESS_HIGH"]),
    ("cur_size", ["DCP{i}_CUR_SIZE"]),
    ("cur_position", ["DCP{i}_CUR_POSITION"]),
    ("cur_hot_spot", ["DCP{i}_CUR_HOT_SPOT"]),
    ("cur_update", ["DCP{i}_CUR_UPDATE"]),
    ("hpd_status", ["HPD{i}_DC_HPD_INT_STATUS"]),
    ("aux_control", ["DP_AUX{i}_AUX_CONTROL"]),
    ("aux_sw_control", ["DP_AUX{i}_AUX_SW_CONTROL"]),
    ("aux_arb_control", ["DP_AUX{i}_AUX_ARB_CONTROL"]),
    ("aux_interrupt_control", ["DP_AUX{i}_AUX_INTERRUPT_CONTROL"]),
    ("aux_sw_status", ["DP_AUX{i}_AUX_SW_STATUS"]),
    ("aux_sw_data", ["DP_AUX{i}_AUX_SW_DATA"]),
]

DCN_VERSIONS = [
    ("DCN10", "1_0", "DCN 1.0 (Raven, Picasso)"),
    ("DCN20", "2_0_0", "DCN 2.0 (Navi 10, 12, 14)"),
    ("DCN21", "2_1_0", "DCN 2.1 (Renoir, Lucienne, Cezanne, Barcelo)"),
    ("DCN30", "3_0_0", "DCN 3.0 (Navi 21, 22)"),
    ("DCN301", "3_0_1", "DCN 3.0.1 (Van Gogh)"),
    ("DCN302", "3_0_2", "DCN 3.0.2 (Navi 23)"),
    ("DCN303", "3_0_3", "DCN 3.0.3 (Navi 24)"),
    ("DCN31", "3_1_2", "DCN 3.1.2 (Rembrandt)"),
    ("DCN314", "3_1_4", "DCN 3.1.4 (Phoenix, Hawk Point)"),
    ("DCN315", "3_1_5", "DCN 3.1.5 (Raphael, Granite Ridge)"),
    ("DCN316", "3_1_6", "DCN 3.1.6 (Mendocino)"),
    ("DCN32", "3_2_0", "DCN 3.2 (Navi 31, 32)"),
    ("DCN321", "3_2_1", "DCN 3.2.1 (Navi 33)"),
]


def load(path):
    regs = {}
    pat = re.compile(r"#define\s+(?:mm|reg)(\w+?)\s+(0x[0-9a-fA-F]+|\d+)\s*$")
    for line in open(path, encoding="latin-1"):
        m = pat.match(line.strip())
        if m:
            regs[m.group(1)] = int(m.group(2), 0)
    return regs


def addr(regs, name, segmented):
    if name not in regs:
        return 0
    if not segmented:
        return regs[name] * 4
    idx = regs.get(name + "_BASE_IDX")
    if idx is None:
        return 0
    return (BASES[idx] + regs[name]) * 4


def emit(struct, table, regs, segmented, n, const, doc):
    print(f"/// {doc}")
    print(f"pub static {const}: {struct} = {struct} {{")
    for field, names in table:
        vals = []
        for i in range(n):
            a = 0
            for t in names:
                a = addr(regs, t.format(i=i), segmented)
                if a:
                    break
            vals.append(a)
        print(f"    {field}: [{', '.join(f'0x{v:05x}' for v in vals)}],")
    print("};\n")


def main():
    d = sys.argv[1]
    print("// Generated by tools/amd-regs.py from AMD's published register headers")
    print("// (asic_reg/dcn/dcn_*_offset.h, asic_reg/dce/dce_11_2_d.h, dce_12_0_offset.h).")
    print("// Byte offsets into the register BAR; 0 means the instance doesn't exist.\n")
    print("use crate::regs::{Dce, Dcn};\n")
    for const, v, doc in DCN_VERSIONS:
        emit("Dcn", DCN, load(f"{d}/dcn_{v}_offset.h"), True, 6, const, doc)
    emit("Dce", DCE, load(f"{d}/dce_11_2_d.h"), False, 6, "DCE112", "DCE 11.2 (Polaris 10, 11, 12)")
    emit("Dce", DCE, load(f"{d}/dce_12_0_offset.h"), True, 6, "DCE120", "DCE 12.0/12.1 (Vega 10, 12, 20)")


main()
