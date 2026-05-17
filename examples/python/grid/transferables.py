import c_two as cc
import pyarrow as pa

ARROW_IPC_CODEC_ID = 'org.apache.arrow.ipc'
ARROW_IPC_CODEC_VERSION = '1'
ARROW_IPC_SCHEMA = 'arrow.ipc.schema.v1'
ARROW_IPC_MEDIA_TYPE = 'application/vnd.apache.arrow.stream'
ARROW_IPC_CAPABILITIES = ('bytes', 'buffer-view')

GRID_SCHEMA_ARROW_SCHEMA = (
    'c-two.examples.grid-schema.arrow-ipc.v1;'
    'fields=epsg:int32,bounds:list<float64>,first_size:list<float64>,'
    'subdivide_rules:list<list<int32>>'
)
GRID_ATTRIBUTE_ARROW_SCHEMA = (
    'c-two.examples.grid-attribute.arrow-ipc.v1;'
    'fields=deleted:bool,activate:bool,type:int8,level:int8,'
    'global_id:int32,local_id:int32?,elevation:float64,'
    'min_x:float64?,min_y:float64?,max_x:float64?,max_y:float64?'
)
GRID_ATTRIBUTE_BATCH_ARROW_SCHEMA = (
    'c-two.examples.grid-attribute-batch.arrow-ipc.v1;'
    'items=c-two.examples.grid-attribute.arrow-ipc.v1;'
    'fields=deleted:bool,activate:bool,type:int8,level:int8,'
    'global_id:int32,local_id:int32?,elevation:float64,'
    'min_x:float64?,min_y:float64?,max_x:float64?,max_y:float64?'
)

GRID_SCHEMA_CODEC_REF = cc.CodecRef.from_schema(
    id=ARROW_IPC_CODEC_ID,
    version=ARROW_IPC_CODEC_VERSION,
    schema=ARROW_IPC_SCHEMA,
    schema_text=GRID_SCHEMA_ARROW_SCHEMA,
    capabilities=ARROW_IPC_CAPABILITIES,
    media_type=ARROW_IPC_MEDIA_TYPE,
)
GRID_ATTRIBUTE_CODEC_REF = cc.CodecRef.from_schema(
    id=ARROW_IPC_CODEC_ID,
    version=ARROW_IPC_CODEC_VERSION,
    schema=ARROW_IPC_SCHEMA,
    schema_text=GRID_ATTRIBUTE_ARROW_SCHEMA,
    capabilities=ARROW_IPC_CAPABILITIES,
    media_type=ARROW_IPC_MEDIA_TYPE,
)
GRID_ATTRIBUTE_BATCH_CODEC_REF = cc.CodecRef.from_schema(
    id=ARROW_IPC_CODEC_ID,
    version=ARROW_IPC_CODEC_VERSION,
    schema=ARROW_IPC_SCHEMA,
    schema_text=GRID_ATTRIBUTE_BATCH_ARROW_SCHEMA,
    capabilities=ARROW_IPC_CAPABILITIES,
    media_type=ARROW_IPC_MEDIA_TYPE,
)


@cc.transferable(
    codec_ref=GRID_SCHEMA_CODEC_REF,
)
class GridSchema:
    """
    Grid Schema
    ---
    - epsg (int): the EPSG code of the grid
    - bounds (list[float]): the bounds of the grid in the format [min_x, min_y, max_x, max_y]
    - first_size (float): the size of the first grid (unit: m)
    - subdivide_rules (list[tuple[int, int]]): the subdivision rules of the grid in the format [(sub_width, sub_height)]
    """
    epsg: int
    bounds: list[float]  # [min_x, min_y, max_x, max_y]
    first_size: list[float]  # [width, height]
    subdivide_rules: list[list[int]]  # [(sub_width, sub_height), ...]
        
    def serialize(grid_schema: 'GridSchema') -> bytes:
        data = {
            'epsg': grid_schema.epsg,
            'bounds': grid_schema.bounds,
            'first_size': grid_schema.first_size,
            'subdivide_rules': grid_schema.subdivide_rules
        }
        
        table = pa.Table.from_pylist([data], schema=grid_schema_arrow_schema())
        return serialize_from_table(table)

    def deserialize(arrow_bytes: bytes) -> 'GridSchema':
        row = deserialize_to_rows(arrow_bytes)[0]
        return GridSchema(
            epsg=row['epsg'],
            bounds=row['bounds'],
            first_size=row['first_size'],
            subdivide_rules=row['subdivide_rules']
        )

@cc.transferable(
    codec_ref=GRID_ATTRIBUTE_CODEC_REF,
)
class GridAttribute:
    """
    Attributes of Grid
    ---
    - level (int8): the level of the grid
    - type (int8): the type of the grid, default to 0
    - activate (bool), the subdivision status of the grid
    - deleted (bool): the deletion status of the grid, default to False
    - elevation (float64): the elevation of the grid, default to -9999.0
    - global_id (int32): the global id within the bounding box that subdivided by grids all in the level of this grid
    - local_id (int32): the local id within the parent grid that subdivided by child grids all in the level of this grid
    - min_x (float64): the min x coordinate of the grid
    - min_y (float64): the min y coordinate of the grid
    - max_x (float64): the max x coordinate of the grid
    - max_y (float64): the max y coordinate of the grid
    """
    level: int
    type: int
    activate: bool
    global_id: int
    deleted: bool = False   
    elevation: float = -9999.0
    local_id: int | None = None
    min_x: float | None = None
    min_y: float | None = None
    max_x: float | None = None
    max_y: float | None = None
    
    def serialize(data: 'GridAttribute') -> bytes:
        table = pa.Table.from_pylist([data.__dict__], schema=grid_attribute_arrow_schema())
        return serialize_from_table(table)
    
    def deserialize(arrow_bytes: bytes) -> 'GridAttribute':
        row = deserialize_to_rows(arrow_bytes)[0]
        return GridAttribute(
            deleted=row['deleted'],
            activate=row['activate'],
            type=row['type'],
            level=row['level'],
            global_id=row['global_id'],
            local_id=row['local_id'],
            elevation=row['elevation'],
            min_x=row['min_x'],
            min_y=row['min_y'],
            max_x=row['max_x'],
            max_y=row['max_y']
        )


@cc.transferable(
    codec_ref=GRID_ATTRIBUTE_BATCH_CODEC_REF,
)
class GridAttributeBatch:
    def serialize(items: list[GridAttribute]) -> bytes:
        rows = [item.__dict__ for item in items]
        table = pa.Table.from_pylist(rows, schema=grid_attribute_arrow_schema())
        return serialize_from_table(table)

    def deserialize(arrow_bytes: bytes) -> list[GridAttribute]:
        rows = deserialize_to_rows(arrow_bytes)
        return [
            GridAttribute(
                deleted=row['deleted'],
                activate=row['activate'],
                type=row['type'],
                level=row['level'],
                global_id=row['global_id'],
                local_id=row['local_id'],
                elevation=row['elevation'],
                min_x=row['min_x'],
                min_y=row['min_y'],
                max_x=row['max_x'],
                max_y=row['max_y'],
            )
            for row in rows
        ]

# Helpers ##################################################


def grid_schema_arrow_schema() -> pa.Schema:
    return pa.schema([
        pa.field('epsg', pa.int32()),
        pa.field('bounds', pa.list_(pa.float64())),
        pa.field('first_size', pa.list_(pa.float64())),
        pa.field('subdivide_rules', pa.list_(pa.list_(pa.int32()))),
    ])


def grid_attribute_arrow_schema() -> pa.Schema:
    return pa.schema([
        pa.field('deleted', pa.bool_()),
        pa.field('activate', pa.bool_()),
        pa.field('type', pa.int8()),
        pa.field('level', pa.int8()),
        pa.field('global_id', pa.int32()),
        pa.field('local_id', pa.int32(), nullable=True),
        pa.field('elevation', pa.float64()),
        pa.field('min_x', pa.float64(), nullable=True),
        pa.field('min_y', pa.float64(), nullable=True),
        pa.field('max_x', pa.float64(), nullable=True),
        pa.field('max_y', pa.float64(), nullable=True),
    ])


def serialize_from_table(table: pa.Table) -> bytes:
    sink = pa.BufferOutputStream()
    with pa.ipc.new_stream(sink, table.schema) as writer:
        writer.write_table(table)
    binary_data = sink.getvalue().to_pybytes()
    return binary_data

def deserialize_to_table(serialized_data: bytes) -> pa.Table:
    buffer = pa.py_buffer(serialized_data)
    with pa.ipc.open_stream(buffer) as reader:
        table = reader.read_all()
    return table

def deserialize_to_rows(serialized_data: bytes) -> list[dict]:
    buffer = pa.py_buffer(serialized_data)

    with pa.ipc.open_stream(buffer) as reader:
        table = reader.read_all()

    return table.to_pylist()
