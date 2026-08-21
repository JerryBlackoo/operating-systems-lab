use libc_types::mman::MmapProt;
use polyhal::MappingFlags;

pub fn map_mprot_to_flags(mprot: MmapProt) -> MappingFlags {
    let mut res = MappingFlags::empty();
    if mprot.contains(MmapProt::READ) {
        res |= MappingFlags::R;
    }
    if mprot.contains(MmapProt::WRITE) {
        res |= MappingFlags::W;
    }
    if mprot.contains(MmapProt::EXEC) {
        res |= MappingFlags::X;
    }
    if res.intersects(MappingFlags::R | MappingFlags::W | MappingFlags::X) {
        res |= MappingFlags::U;
    }
    res
}
